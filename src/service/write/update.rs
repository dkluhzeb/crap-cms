//! Core update operation for collections.

use crate::core::validate::{FieldError, ValidationError};
use crate::core::{
    CollectionDefinition, DocumentFields, flatten_group_fields, prefixed_name, walk_leaf_fields,
};
use crate::db::LocaleMode;

use crate::{
    db::{AccessResult, LocaleContext, query},
    hooks::{AccessCheckInput, HookContext, ValidationCtx},
    service::{
        AfterChangeInput, PersistOptions, ServiceContext, WriteInput, WriteResult,
        persist_draft_version, persist_update, run_after_change_hooks,
    },
};

use super::ServiceError;
use crate::core::nest_group_fields;
use crate::service::helpers::{
    collect_api_hidden_field_names, enforce_access_constraints, validate_password_policy,
};
use crate::service::hooks::WriteHooks;

type Result<T> = std::result::Result<T, ServiceError>;

/// The collection-level `update` access gate — ONE chokepoint shared by the
/// real update and the update-mode dry-run ([`op::Validate`]), so the two can
/// never drift. Callers pass canonicalized (group-nested) data — the access
/// function sees it as `ctx.data` with the target `id`. A `Constrained`
/// return (e.g. "only rows where `author_id` = me") is enforced against the
/// target row.
///
/// [`op::Validate`]: crate::service::op::Validate
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_update_access(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    def: &crate::core::CollectionDefinition,
    id: &str,
    data: &crate::core::DocumentFields,
    locale: Option<&str>,
    ui_locale: Option<&str>,
) -> Result<()> {
    let access = write_hooks.check_access(
        &AccessCheckInput::builder("update", ctx.slug)
            .access(def.access.update.as_ref())
            .user(ctx.user)
            .id(Some(id))
            .data(Some(data))
            .locale(locale)
            .ui_locale(ui_locale)
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    enforce_access_constraints(ctx, id, &access, "Update", false)?;

    Ok(())
}

/// Reject a non-default-locale write that includes locale-locked fields.
///
/// A field that is not localized has exactly one (default-locale) column, so a
/// write under `locale = "de"` cannot store it. These fields used to be
/// **silently dropped** — the write succeeded while discarding data, the worst
/// possible outcome. They are now a validation error naming each field; write
/// shared fields under the default locale instead.
pub(crate) fn reject_locale_locked_fields(
    def: &CollectionDefinition,
    data: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    let Some(lctx) = locale_ctx else {
        return Ok(());
    };
    let LocaleMode::Single(locale) = &lctx.mode else {
        return Ok(());
    };
    if *locale == lctx.config.default_locale {
        return Ok(());
    }

    // Data arrives nested-canonical; flatten so presence checks use the same
    // `group__sub` names the leaf walker produces.
    let flat = flatten_group_fields(data, &def.fields);

    let mut errors = Vec::new();
    let _ = walk_leaf_fields(&def.fields, "", false, &mut |field, prefix, inherited| {
        if query::is_locale_locked_write(field, Some(lctx), inherited) {
            let key = prefixed_name(prefix, &field.name);

            if flat.contains_key(&key) {
                errors.push(FieldError::new(
                    key,
                    format!(
                        "not localized — this field only exists under the default locale \
                         ('{}'); drop it from the '{locale}' write or mark it localized",
                        lctx.config.default_locale
                    ),
                ));
            }
        }
        Ok(())
    });

    if errors.is_empty() {
        return Ok(());
    }

    Err(ValidationError::new(errors).into())
}

/// Update a document on an existing connection/transaction.
///
/// Runs the full lifecycle: before-write hooks -> persist -> after-write hooks.
/// Handles draft-only version saves when `input.draft` is true.
/// Does NOT manage transactions — caller must open/commit.
pub(crate) fn update_document_in_conn(
    ctx: &ServiceContext,
    id: &str,
    mut input: WriteInput<'_>,
) -> Result<WriteResult> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    // Canonicalize incoming data to nested groups up front (idempotent); the
    // whole pipeline sees one shape, the DB edge flattens to columns.
    input.data = nest_group_fields(&input.data, &def.fields);

    // Drop server-derived upload columns from untrusted input (all surfaces
    // but the multipart upload handlers) so `url`/`*_url` can't be forged.
    super::validate::strip_untrusted_upload_metadata(&mut input, def);

    reject_locale_locked_fields(def, &input.data, input.locale_ctx)?;

    check_update_access(
        ctx,
        write_hooks,
        def,
        id,
        &input.data,
        input.locale_ctx.map(LocaleContext::access_locale),
        input.ui_locale.as_deref(),
    )?;

    // Authoritative password-policy enforcement (all surfaces): a weak password
    // on an auth-collection update is rejected here as a `password` field error.
    // An empty password means "no change" and is skipped. `ctx.password_policy`
    // falls back to the default policy, so this can never silently skip.
    validate_password_policy(
        def.is_auth_collection(),
        input.password,
        ctx.password_policy,
    )?;

    let is_draft = input.draft && def.has_drafts();
    let ui_locale = input.ui_locale.as_deref();

    // Strip write-denied fields before hook processing (data-aware: each
    // `access.update` rule sees `ctx.data` = its level and `ctx.document` = the
    // full incoming document).
    write_hooks.strip_write_access_data(
        &def.fields,
        &mut input.data,
        ctx.slug,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
        "update",
    );

    let hook_data = input.data.clone();

    let hook_ctx = HookContext::builder(ctx.slug, "update")
        .data(hook_data)
        .document_id(id)
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
        .draft(is_draft)
        .user(ctx.user)
        .ui_locale(ui_locale)
        .build();

    let val_ctx = ValidationCtx::builder(conn, ctx.slug)
        .exclude_id(Some(id))
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .collection_required_locales(def.required_locales.as_ref())
        .user(ctx.user)
        .ui_locale(input.ui_locale.as_deref())
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;

    let doc = if is_draft && def.has_versions() {
        persist_draft_version(ctx, id, &final_ctx.data, input.locale_ctx)?
    } else {
        let opts = PersistOptions::builder()
            .password(input.password)
            .locale_ctx(input.locale_ctx)
            .locale_config(input.locale_ctx.map(|c| &c.config))
            .build();

        persist_update(ctx, id, &final_ctx.to_value_map(), &opts)?
    };

    // Hydrate join fields (arrays, blocks, has-many) BEFORE after-change hooks so
    // they can react to nested array/blocks/has-many data, not just scalar columns.
    let mut doc = doc;

    query::hydrate_document(
        conn,
        ctx.slug,
        &def.fields,
        &mut doc,
        None,
        input.locale_ctx,
    )?;

    let after_ctx = run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "update")
            .locale(
                input
                    .locale_ctx
                    .map(LocaleContext::access_locale)
                    .map(String::from),
            )
            .draft(is_draft)
            .req_context(final_ctx.context)
            .user(ctx.user)
            .ui_locale(ui_locale)
            .build(),
        conn,
    )?;

    // NOTE: auth-document edits invalidate a user's live-update streams, but
    // that is published POST-COMMIT by the orchestrators (`update_document_pool`
    // / `update_many_pool` and their conn variants) on the outer context that
    // carries the invalidation transport — mirroring `publish_mutation_event`.
    // Doing it here (inner ctx, pre-commit) was both a no-op and unsafe on
    // rollback.

    // Strip read-denied fields from the returned document, after the hooks have
    // seen the full doc.
    let access_locale = input.locale_ctx.map(LocaleContext::access_locale);
    write_hooks.strip_read_access_doc(&def.fields, &mut doc, ctx.slug, ctx.user, access_locale);
    doc.strip_fields(&collect_api_hidden_field_names(&def.fields, ""));

    Ok((doc, after_ctx))
}

#[cfg(test)]
mod locale_lock_tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::{CollectionDefinition, DocumentFields, FieldDefinition, field::FieldType},
    };

    fn def_with_shared_and_localized() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("slug", FieldType::Text).build(),
        ];
        def
    }

    fn ctx(locale: &str) -> LocaleContext {
        LocaleContext {
            mode: LocaleMode::Single(locale.to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        }
    }

    /// Regression: a non-default-locale write carrying a non-localized field
    /// used to succeed while silently discarding that field.
    #[test]
    fn non_default_locale_write_rejects_locale_locked_fields() {
        let def = def_with_shared_and_localized();
        let data: DocumentFields = [
            ("title".to_string(), json!("Titel")),
            ("slug".to_string(), json!("neu")),
        ]
        .into_iter()
        .collect();

        let err = reject_locale_locked_fields(&def, &data, Some(&ctx("de"))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("slug"), "{msg}");
        assert!(msg.contains("not localized"), "{msg}");

        let localized_only: DocumentFields = [("title".to_string(), json!("Titel"))]
            .into_iter()
            .collect();
        assert!(reject_locale_locked_fields(&def, &localized_only, Some(&ctx("de"))).is_ok());
        assert!(reject_locale_locked_fields(&def, &data, Some(&ctx("en"))).is_ok());
        assert!(reject_locale_locked_fields(&def, &data, None).is_ok());
    }
}
