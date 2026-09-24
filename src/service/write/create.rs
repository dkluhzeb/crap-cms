//! Core create operation for collections.

use crate::{
    core::{CollectionDefinition, DocumentFields},
    db::{AccessResult, LocaleContext},
    hooks::{AccessCheckInput, HookContext, ValidationCtx},
    service::{
        AfterChangeInput, Gated, PersistOptions, ServiceContext, WriteInput, WriteResult,
        persist_create, run_after_change_hooks,
        write::{UploadSettle, admit_create_input, settle_upload_write},
    },
};

use super::ServiceError;
use crate::service::helpers::{
    EmptyPassword, hydrate_reported, strip_reported, validate_password_policy,
};
use crate::service::hooks::WriteHooks;

type Result<T> = std::result::Result<T, ServiceError>;

/// The collection-level `create` access gate — ONE chokepoint shared by the
/// real create and the create-mode dry-run ([`op::Validate`]), so the two can
/// never drift. Callers pass canonicalized (group-nested) data — the access
/// function sees it as `ctx.data`.
///
/// `Constrained` returns make no sense for create: there is no target row to
/// match against, and evaluating the filter against the incoming data would
/// conflate access control with validation — operators should return
/// true/false based on `ctx.data` instead.
///
/// [`op::Validate`]: crate::service::op::Validate
pub(crate) fn check_create_access(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    def: &CollectionDefinition,
    data: &DocumentFields,
    locale: Option<&str>,
    ui_locale: Option<&str>,
) -> Result<()> {
    let access = write_hooks.check_access(
        &AccessCheckInput::builder("create", ctx.slug)
            .access(def.access.create.as_ref())
            .user(ctx.user)
            .data(Some(data))
            .locale(locale)
            .ui_locale(ui_locale)
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Create access denied".into()));
    }

    if matches!(access, AccessResult::Constrained(_)) {
        return Err(ServiceError::HookError(format!(
            "Access hook for '{}.create' returned a filter table; filter-table returns are only valid for update/delete/undelete/unpublish (where a target row exists). Return true/false based on the incoming 'data' in ctx.",
            ctx.slug
        )));
    }

    Ok(())
}

/// Create a document on an existing connection/transaction.
///
/// Runs the full lifecycle: before-write hooks -> persist -> after-write hooks.
/// Does NOT manage transactions — caller must open/commit.
///
/// # Errors
///
/// Returns service-layer errors (access denied, validation, hook errors) or
/// a backend error if persistence fails.
pub fn create_document_in_conn(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    create_document_gated(ctx, input).map(|(result, _)| result)
}

/// [`create_document_in_conn`], plus the stored row the create's live event is
/// built from — for the service entry points that publish it.
///
/// # Errors
///
/// As [`create_document_in_conn`].
pub(crate) fn create_document_gated(
    ctx: &ServiceContext,
    mut input: WriteInput<'_>,
) -> Result<Gated<WriteResult>> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    // The admission prefix the `validate` dry-run runs too: canonicalize the
    // incoming data to the nested group shape (every surface and the whole
    // pipeline sees one shape; the DB edge flattens back to columns), strip
    // untrusted upload metadata, and refuse a non-default locale.
    admit_create_input(def, &mut input)?;

    check_create_access(
        ctx,
        write_hooks,
        def,
        &input.data,
        input.locale_ctx.map(LocaleContext::access_locale),
        input.ui_locale.as_deref(),
    )?;

    // Authoritative password-policy enforcement — one chokepoint for every
    // surface and every create path (single AND `create_many`); falls back to
    // the default policy so it can never silently skip. A present-but-empty
    // password is a caller error here: on create there is nothing to leave
    // alone, so treating it as "no change" would quietly produce a
    // passwordless account. See `validate_password_policy`.
    validate_password_policy(
        def.is_auth_collection(),
        input.password,
        ctx.password_policy,
        EmptyPassword::IsRejected,
    )?;

    let is_draft = input.draft && def.has_drafts();
    let ui_locale = input.ui_locale.as_deref();

    // Strip write-denied fields before hook processing (data-aware: each
    // `access.create` rule sees `ctx.data` = its level and `ctx.document` = the
    // full incoming document — no row exists yet).
    write_hooks.strip_write_access_create(
        &def.fields,
        &mut input.data,
        ctx.slug,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
    );

    let hook_ctx = HookContext::builder(ctx.slug, "create")
        .data(input.data.clone())
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
        .draft(is_draft)
        .user(ctx.user)
        .ui_locale(ui_locale)
        .build();

    let val_ctx = ValidationCtx::builder(conn, ctx.slug)
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .collection_required_locales(def.required_locales.as_ref())
        .user(ctx.user)
        .ui_locale(input.ui_locale.as_deref())
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_value_map();

    let opts = PersistOptions::builder()
        .password(input.password)
        .locale_ctx(input.locale_ctx)
        .locale_config(input.locale_ctx.map(|c| &c.config))
        .draft(is_draft)
        .build();

    let mut doc = persist_create(ctx, &final_data, &opts)?;

    // The new file's conversions go into the queue on THIS connection, inside
    // the write transaction — a create has no previous file to clean up.
    settle_upload_write(
        ctx,
        &UploadSettle::builder(def, &doc.id)
            .conversions(input.upload_conversions.as_ref())
            .build(),
    )?;

    // Hydrate join fields (arrays, blocks, has-many) BEFORE after-change hooks so
    // they can react to nested array/blocks/has-many data, not just scalar columns.
    hydrate_reported(ctx, &mut doc, input.locale_ctx)?;

    let after_ctx = run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "create")
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

    // The row as stored, before anything is shaped or stripped for the writer:
    // the live event is built from it.
    let row = ctx.write_event_row(&doc, input.locale_ctx, false)?;

    // Strip read-denied fields from the returned document, after the hooks have
    // seen the full doc (hydration can add join data for denied fields).
    strip_reported(ctx, write_hooks, &mut doc, input.locale_ctx)?;

    Ok(((doc, after_ctx), row))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    use anyhow::Result as AnyResult;
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::{
        config::CrapConfig,
        core::{
            Document, FieldAccess, FieldDefinition, FieldType, HookRef, Hooks, LiveMode, Registry,
            SharedEventTransport, ValidationError, event::InProcessEventBus,
        },
        db::{DbConnection, DbPool, Filter, FilterClause, FilterOp, migrate, pool},
        hooks::HookEvent,
        service::{EventQueue, FieldReadStrip, create_document, update_document_gated},
    };

    /// Write hooks that run nothing and allow every access check — except that
    /// the writer may not read `notes`.
    struct AllowAll;

    impl WriteHooks for AllowAll {
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

    impl FieldReadStrip for AllowAll {
        fn strip_read_access_map(
            &self,
            _fields: &[FieldDefinition],
            level: &mut Map<String, Value>,
            _document: &DocumentFields,
            _collection: &str,
            _user: Option<&Document>,
            _locale: Option<&str>,
        ) {
            level.remove("notes");
        }
    }

    /// A migrated `posts` collection whose `owner` is API-hidden — stripped
    /// from every read, yet a row constraint may still filter on it — and
    /// whose `notes` carries a read rule (which [`AllowAll`] denies).
    fn migrated_posts() -> (tempfile::TempDir, DbPool, CollectionDefinition) {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("owner", FieldType::Text)
                .hidden(true)
                .build(),
            FieldDefinition::builder("notes", FieldType::Text)
                .access(FieldAccess {
                    read: Some(HookRef::new("hooks.notes.read")),
                    ..Default::default()
                })
                .build(),
        ];

        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

        let shared = Registry::shared();
        shared.write().unwrap().register_collection(def.clone());
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &config.locale).unwrap();

        (tmp, db_pool, def)
    }

    fn owner_is(owner: &str) -> [FilterClause; 1] {
        [FilterClause::Single(Filter {
            field: "owner".to_string(),
            op: FilterOp::Equals(owner.to_string()),
        })]
    }

    /// The gating snapshot is the row as stored — the hidden `owner` a row
    /// constraint names included — while the reported document is stripped
    /// for the writer as before. Judging the stripped report instead dropped
    /// every event for a view constrained on a hidden field.
    #[test]
    fn create_and_update_gate_on_the_unstripped_row() {
        let (_tmp, db_pool, def) = migrated_posts();
        let conn = db_pool.get().unwrap();
        let hooks = AllowAll;
        let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .event_transport(Some(transport))
            .build();

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello"));
        data.insert("owner".to_string(), json!("u1"));

        let ((created, _), row) =
            create_document_gated(&ctx, WriteInput::builder(data).build()).unwrap();

        assert!(
            created.fields.get("owner").is_none(),
            "{:?}",
            created.fields
        );
        let gate = row
            .expect("a publishing create carries its row")
            .gate_snapshot();
        assert!(gate.matches(&owner_is("u1"), &def.fields));
        assert!(!gate.matches(&owner_is("u2"), &def.fields));

        let mut patch = DocumentFields::new();
        patch.insert("title".to_string(), json!("Edited"));

        let ((updated, _), row) =
            update_document_gated(&ctx, &created.id, WriteInput::builder(patch).build()).unwrap();

        assert!(
            updated.fields.get("owner").is_none(),
            "{:?}",
            updated.fields
        );
        let gate = row
            .expect("a publishing update carries its row")
            .gate_snapshot();
        assert!(gate.matches(&owner_is("u1"), &def.fields));
    }

    /// Regression: a `Full`-mode event carried the document as stripped for
    /// the WRITER, so a subscriber allowed a field the writer may not read
    /// never received it. The writer's report still loses the field; the event
    /// carries the stored row, hidden fields included, for each subscriber's
    /// own delivery strip.
    #[test]
    fn a_full_mode_event_is_not_stripped_for_the_writer() {
        let (_tmp, db_pool, mut def) = migrated_posts();
        def.live_mode = LiveMode::Full;

        let conn = db_pool.get().unwrap();
        let hooks = AllowAll;
        let queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .event_transport(Some(transport))
            .event_queue(queue.clone())
            .build();

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello"));
        data.insert("owner".to_string(), json!("u1"));
        data.insert("notes".to_string(), json!("internal"));

        let (created, _) = create_document(&ctx, WriteInput::builder(data).build()).unwrap();

        assert!(
            created.fields.get("notes").is_none(),
            "{:?}",
            created.fields
        );
        assert!(
            created.fields.get("owner").is_none(),
            "{:?}",
            created.fields
        );

        let queued = queue.borrow();
        let event = queued.first().expect("event queued");

        assert_eq!(event.mode, LiveMode::Full);
        assert_eq!(event.data.get_str("notes"), Some("internal"));
        assert_eq!(event.data.get_str("owner"), Some("u1"));
        assert_eq!(event.data.get_str("title"), Some("Hello"));
    }
}
