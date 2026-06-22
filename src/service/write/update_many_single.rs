//! Core per-document update for bulk operations (partial update, no password).
//! Honors `draft` the same way the single-document update does: a draft save
//! is routed to the version table and leaves the published main row untouched.

use crate::{
    config::LocaleConfig,
    db::{AccessResult, LocaleContext, query},
    hooks::{AccessCheckInput, HookContext, ValidationCtx},
    service::{
        AfterChangeInput, ServiceContext, WriteInput, WriteResult, persist_bulk_update,
        persist_draft_version, run_after_change_hooks,
    },
};

use super::ServiceError;
use crate::core::nest_group_fields;
use crate::service::helpers::{collect_api_hidden_field_names, enforce_access_constraints};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a single document in a bulk operation (partial update).
///
/// Runs the full lifecycle: access check -> field stripping -> before-write hooks ->
/// partial persist -> hydrate -> after-write hooks -> read-denied stripping.
/// Does NOT manage transactions — caller must open/commit.
pub(crate) fn update_many_single_in_conn(
    ctx: &ServiceContext,
    id: &str,
    mut input: WriteInput<'_>,
    locale_config: &LocaleConfig,
) -> Result<WriteResult> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    // Canonicalize incoming data to nested groups up front (idempotent).
    input.data = nest_group_fields(&input.data, &def.fields);

    let access = write_hooks.check_access(&AccessCheckInput {
        document: None,
        access: def.access.update.as_ref(),
        user: ctx.user,
        id: Some(id),
        data: Some(&input.data),
        locale: input.locale_ctx.map(LocaleContext::access_locale),
        operation: "update",
        collection: ctx.slug,
        ui_locale: input.ui_locale.as_deref(),
    })?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    // When the hook returned Constrained filters, enforce row-level match.
    enforce_access_constraints(ctx, id, &access, "Update", false)?;

    let is_draft = input.draft && def.has_drafts();

    // Data-aware write strip (per-row `ctx.data`, full-doc `ctx.document`).
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
        .ui_locale(input.ui_locale.as_deref())
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

    // A draft bulk update routes to the version table (main row untouched),
    // exactly like the single-document update path — otherwise `draft = true`
    // would silently publish the change by writing the main row.
    let mut doc = if is_draft && def.has_versions() {
        persist_draft_version(ctx, id, &final_ctx.data, input.locale_ctx)?
    } else {
        let final_data = final_ctx.to_value_map();
        persist_bulk_update(ctx, id, &final_data, input.locale_ctx, locale_config)?
    };

    // Hydrate join fields BEFORE after-change hooks so they see nested data.
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
            .ui_locale(input.ui_locale.as_deref())
            .build(),
        conn,
    )?;

    let access_locale = input.locale_ctx.map(LocaleContext::access_locale);
    write_hooks.strip_read_access_doc(&def.fields, &mut doc, ctx.slug, ctx.user, access_locale);
    doc.strip_fields(&collect_api_hidden_field_names(&def.fields, ""));

    Ok((doc, after_ctx))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result;
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::{
        core::{
            CollectionDefinition, DocumentFields, FieldDefinition, FieldType, Hooks,
            ValidationError, collection::VersionsConfig,
        },
        db::{AccessResult, DbConnection},
        hooks::{HookContext, HookEvent, ValidationCtx},
        service::hooks::WriteHooks,
    };

    struct NoopWriteHooks;

    impl WriteHooks for NoopWriteHooks {
        fn run_before_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            ctx: HookContext,
            _val_ctx: &ValidationCtx,
        ) -> Result<HookContext> {
            Ok(ctx)
        }

        fn run_after_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> Result<HookContext> {
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _hooks: &Hooks,
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> Result<HookContext> {
            Ok(ctx)
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }

        fn validate_fields(
            &self,
            _fields: &[FieldDefinition],
            _data: &DocumentFields,
            _ctx: &ValidationCtx,
        ) -> std::result::Result<(), ValidationError> {
            Ok(())
        }
    }

    fn versioned_collection() -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                _ref_count INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT
            );
            INSERT INTO posts (id, title, _status) VALUES ('p1', 'Original', 'published');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.versions = Some(VersionsConfig::new(true, 0));

        (conn, def)
    }

    /// Regression: a bulk update with `draft = true` on a versioned collection
    /// must route to the version table and leave the published main row
    /// untouched — exactly like the single-document update. Before the fix the
    /// bulk path wrote the main row, silently publishing the change.
    #[test]
    fn bulk_draft_update_does_not_touch_published_main_row() {
        let (conn, def) = versioned_collection();
        let wh = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&wh)
            .build();

        let mut data = DocumentFields::new();
        data.insert("title".into(), json!("Edited"));
        let input = WriteInput::builder(data).draft(true).build();

        update_many_single_in_conn(&ctx, "p1", input, &LocaleConfig::default()).unwrap();

        // Main row is unchanged and still published.
        let row = DbConnection::query_one(
            &conn,
            "SELECT title, _status FROM posts WHERE id = 'p1'",
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(row.get_string("title").unwrap(), "Original");
        assert_eq!(row.get_string("_status").unwrap(), "published");

        // A draft version captured the edit.
        let versions = query::list_versions(&conn, "posts", "p1", false, None, None).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].status, "draft");
        assert_eq!(
            versions[0].snapshot.get("title").and_then(|v| v.as_str()),
            Some("Edited")
        );
    }
}
