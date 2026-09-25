//! Core update operation for collections.

use serde_json::Value;

use crate::core::validate::{FieldError, ValidationError};
use crate::core::{CollectionDefinition, DocumentFields, FieldDefinition, flatten_group_fields};
use crate::db::LocaleMode;

use crate::{
    db::{AccessResult, DbConnection, LocaleContext, query},
    hooks::{AccessCheckInput, ValidationCtx, lifecycle::access::has_any_field_access},
    service::{
        AfterChangeInput, Gated, PersistOptions, ServiceContext, WriteInput, WriteResult,
        persist_draft_version, persist_update, run_after_change_hooks,
        write::{UploadSettle, admit::admit_update, document_file_keys, settle_upload_write},
    },
};

use super::ServiceError;
use crate::service::helpers::{
    EmptyPassword, enforce_access_constraints, hydrate_reported, strip_reported,
    validate_password_policy,
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
    def: &CollectionDefinition,
    id: &str,
    data: &DocumentFields,
    locale: Option<&str>,
) -> Result<()> {
    let access = write_hooks.check_access(
        &AccessCheckInput::builder("update", ctx.slug)
            .access(def.access.update.as_ref())
            .user(ctx.user)
            .id(Some(id))
            .data(Some(data))
            .locale(locale)
            .ui_locale(ctx.ui_locale.as_deref())
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    enforce_access_constraints(ctx, id, &access, "Update", false)?;

    Ok(())
}

/// Reject a non-default-locale write that carries a locale-locked (shared)
/// field.
///
/// A shared field lives in one column regardless of locale; the DB edge skips
/// it under a non-default locale so a translation can never clobber the
/// canonical value. Skipping silently would mean the write succeeds while
/// discarding data — the worst possible outcome for a programmatic caller
/// (gRPC/Lua/MCP) — so the presence of such a field is a validation error.
/// The locked set is the SAME one the draft snapshot and the admin form strip
/// use (`locale_locked_field_names`: scalar inheritance-aware, join-backed
/// own-flag-only, tz companions), so every surface agrees on what "locked"
/// means.
///
/// Called twice per write with different roles: on the caller's input
/// (early, before hooks) and on the final post-hook data at persist time,
/// where a `before_change` hook that injected a shared field is caught the
/// same way — never silently dropped.
///
/// # Errors
///
/// Returns a `ValidationError` naming every locked field present in `data`.
pub(crate) fn reject_locale_locked_fields(
    fields: &[FieldDefinition],
    data: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    let locked = query::locale_locked_field_names(fields, locale_ctx);
    if locked.is_empty() {
        return Ok(());
    }
    let Some(LocaleContext {
        mode: LocaleMode::Single(locale),
        config,
    }) = locale_ctx
    else {
        return Ok(());
    };

    // Data arrives nested-canonical; flatten so presence checks use the same
    // `group__sub` names the locked set carries.
    let flat = flatten_group_fields(data, fields);

    let mut present: Vec<&String> = flat.keys().filter(|k| locked.contains(*k)).collect();
    if present.is_empty() {
        return Ok(());
    }
    present.sort();

    let errors = present
        .into_iter()
        .map(|key| {
            FieldError::new(
                key.clone(),
                format!(
                    "not localized — this field only exists under the default locale \
                     ('{}'); drop it from the '{locale}' write or mark it localized",
                    config.default_locale
                ),
            )
        })
        .collect();

    Err(ValidationError::new(errors).into())
}

/// Load the stored document that field-level `access.update` rules judge as
/// `ctx.document`.
///
/// Those rules decide whether the caller may change a field, so they must see
/// the row as it stands — never the incoming patch, which the caller controls.
/// Skips the read when no field configures `access.update`. A missing row
/// yields an empty document, so a rule keyed on stored values denies.
///
/// # Errors
///
/// Returns an error if the row cannot be read.
pub(crate) fn stored_fields_for_update_rules(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<DocumentFields> {
    if !has_any_field_access(&def.fields, |f| f.access.update.as_ref()) {
        return Ok(DocumentFields::default());
    }

    let stored = query::find_by_id(conn, slug, def, id, locale_ctx)?;

    Ok(stored.map(|doc| doc.fields).unwrap_or_default())
}

/// Update a document on an existing connection/transaction.
///
/// Authoritative password-policy enforcement for an update (all surfaces): a
/// weak password on an auth-collection update is rejected as a `password`
/// field error. An empty password means "no change" and is skipped;
/// `ctx.password_policy` falls back to the default policy, so this can never
/// silently skip.
fn check_update_password(
    ctx: &ServiceContext,
    def: &CollectionDefinition,
    password: Option<&str>,
) -> Result<()> {
    validate_password_policy(
        def.is_auth_collection(),
        password,
        ctx.password_policy,
        EmptyPassword::MeansNoChange,
    )
}

/// [`update_document_gated`] without the event snapshot — the shape the unit
/// tests drive the update lifecycle through. Does NOT manage transactions —
/// caller must open/commit.
#[cfg(test)]
pub(crate) fn update_document_in_conn(
    ctx: &ServiceContext,
    id: &str,
    input: WriteInput<'_>,
) -> Result<WriteResult> {
    update_document_gated(ctx, id, input).map(|(result, _)| result)
}

/// Runs the full lifecycle: before-write hooks -> persist -> after-write hooks,
/// handling draft-only version saves when `input.draft` is true, and returns
/// the result plus the stored row the update's live event is built from — the
/// written row, or for a draft save the draft snapshot it reports —
/// for the service entry points that publish it.
pub(crate) fn update_document_gated(
    ctx: &ServiceContext,
    id: &str,
    mut input: WriteInput<'_>,
) -> Result<Gated<WriteResult>> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    let publishing_draft = admit_update(ctx, conn, id, &mut input)?;

    check_update_password(ctx, def, input.password)?;

    let is_draft = input.draft && def.has_drafts();
    let ui_locale = ctx.ui_locale.as_deref();

    let hook_data = input.data.clone();

    let hook_ctx = ctx
        .hook_context("update")
        .data(hook_data)
        .document_id(id)
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
        .draft(is_draft)
        .build();

    // A publish writes the draft's other locales back over the row after this
    // validation, so the completeness gate judges that snapshot rather than the
    // locales it is about to replace.
    let val_ctx = ValidationCtx::builder(conn, ctx.slug)
        .exclude_id(Some(id))
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .collection_required_locales(def.required_locales.as_ref())
        .user(ctx.user)
        .ui_locale(ui_locale)
        .locale_overlay(publishing_draft.as_ref().and_then(Value::as_object))
        .versioned_drafts(def.has_drafts())
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;

    // A draft save writes a version snapshot and leaves the published row —
    // and every file it references — exactly as it was.
    let snapshot_only = is_draft && def.has_versions();

    // The files the document references going in — the published row's and
    // every version snapshot's — so the ones nothing references any more can be
    // dropped once the write has landed. A draft-only save needs them too: it
    // can prune the snapshot that was the last reference to an earlier draft's
    // file. Read last, so a before-hook that rewrote the row through its own
    // CRUD is accounted for.
    let before_files = document_file_keys(ctx, def, id, input.locale_ctx)?;

    // The row as it goes in, read as late as the files are: a publish of a
    // draft moves it out of the draft view — content and all — which the live
    // event announces, and a draft save leaves it where it is.
    let row_before = ctx.update_row_before(id, snapshot_only, input.locale_ctx)?;

    // A draft save reports its snapshot, read for the write's locale with its own
    // rows. A published write reports the stored row, its join fields (arrays,
    // blocks, has-many) hydrated BEFORE after-change hooks so they can react to
    // nested data, not just scalar columns.
    let mut doc = if snapshot_only {
        let doc = persist_draft_version(ctx, id, &final_ctx.data, input.locale_ctx)?;

        // The published row, its files and their queued conversions are
        // untouched; the drafted file's own conversions wait for the publish
        // that makes it live. What can still go is a file whose last reference
        // was a snapshot this save's pruning removed.
        settle_upload_write(
            ctx,
            &UploadSettle::builder(def, id)
                .before(Some(&before_files))
                .build(),
        )?;

        doc
    } else {
        let opts = PersistOptions::builder()
            .password(input.password)
            .locale_ctx(input.locale_ctx)
            .locale_config(input.locale_ctx.map(|c| &c.config))
            .pending_draft(publishing_draft.as_ref().and_then(Value::as_object))
            .build();

        let mut doc = persist_update(ctx, id, &final_ctx.to_value_map(), &opts)?;

        settle_upload_write(
            ctx,
            &UploadSettle::builder(def, id)
                .before(Some(&before_files))
                .updated_row(Some(&doc.fields))
                .conversions(input.upload_conversions.as_ref())
                .build(),
        )?;

        hydrate_reported(ctx, &mut doc, input.locale_ctx)?;
        doc
    };

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

    // The row as stored, before anything is shaped or stripped for the writer:
    // the live event is built from it, with the status view it moved from.
    let row = ctx
        .write_event_row(&doc, input.locale_ctx, snapshot_only)?
        .map(|row| row.before_write(row_before, snapshot_only));

    // Strip read-denied fields from the returned document, after the hooks have
    // seen the full doc.
    strip_reported(ctx, write_hooks, &mut doc, input.locale_ctx)?;

    Ok(((doc, after_ctx), row))
}

#[cfg(all(test, feature = "sqlite"))]
mod write_lock_tests {
    use anyhow::Result as AnyResult;
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::{
        core::{FieldType, Hooks, upload::CollectionUpload},
        db::query::test_helpers::CountingConn,
        hooks::{HookContext, HookEvent},
        service::FieldReadStrip,
    };

    /// Write hooks that run nothing and allow every access check.
    struct NoopWriteHooks;

    impl WriteHooks for NoopWriteHooks {
        fn run_before_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            ctx: HookContext,
            _val_ctx: &ValidationCtx,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_after_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _hooks: &Hooks,
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> AnyResult<AccessResult> {
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

    impl FieldReadStrip for NoopWriteHooks {}

    /// An upload collection with one editable field beside the file columns.
    fn media() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload::new());
        def.fields = vec![
            FieldDefinition::builder("url", FieldType::Text).build(),
            FieldDefinition::builder("filename", FieldType::Text).build(),
            FieldDefinition::builder("caption", FieldType::Text).build(),
        ];

        def
    }

    /// One stored upload document pointing at one stored file.
    fn media_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE media (
                id TEXT PRIMARY KEY,
                url TEXT,
                filename TEXT,
                caption TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO media (id, url, filename)
                VALUES ('m1', '/uploads/media/a.png', 'a.png');",
        )
        .unwrap();

        conn
    }

    /// The write locks the document row before it reads the files the document
    /// references. That snapshot decides which stored files the write leaves
    /// unreferenced, so two unlocked replacements of the same document's file
    /// each miss the other's new key and one uploaded file is never recognised
    /// as droppable — its bytes stay in storage forever.
    #[test]
    fn an_update_locks_the_row_before_it_reads_the_files_it_may_drop() {
        let conn = media_db();
        let spy = CountingConn::new(&conn);
        let def = media();
        let hooks = NoopWriteHooks;
        let ctx = ServiceContext::collection("media", &def)
            .conn(&spy)
            .write_hooks(&hooks)
            .build();

        let mut data = DocumentFields::new();
        data.insert("caption".to_string(), json!("new"));

        let (doc, _) =
            update_document_in_conn(&ctx, "m1", WriteInput::builder(data).build()).unwrap();

        assert_eq!(doc.get_str("caption"), Some("new"));
        assert_eq!(
            spy.locks().first(),
            Some(&("media".to_string(), "m1".to_string()))
        );
        assert_eq!(
            spy.reads_at_locks().first(),
            Some(&0),
            "the write reads nothing before it locks the row — the file \
             snapshot included"
        );
        assert!(spy.reads() > 1, "the write read the row and its files");
    }
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

        let err = reject_locale_locked_fields(&def.fields, &data, Some(&ctx("de"))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("slug"), "{msg}");
        assert!(msg.contains("not localized"), "{msg}");

        let localized_only: DocumentFields = [("title".to_string(), json!("Titel"))]
            .into_iter()
            .collect();
        assert!(
            reject_locale_locked_fields(&def.fields, &localized_only, Some(&ctx("de"))).is_ok()
        );
        assert!(reject_locale_locked_fields(&def.fields, &data, Some(&ctx("en"))).is_ok());
        assert!(reject_locale_locked_fields(&def.fields, &data, None).is_ok());
    }
}
