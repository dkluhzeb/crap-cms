//! Global document unpublish.

use crate::{
    core::{Document, EventViewPlacement, GlobalDefinition, event::EventOperation},
    db::{query, query::helpers::global_table},
    hooks::AccessCheckInput,
    service::{
        Gated, ServiceContext, ServiceError, StateChange, global_access_allowed, helpers,
        require_unpublish_capability, run_after_change_hooks, run_pool_write,
        run_state_before_change, unpublish_with_snapshot, versions::VersionSnapshotCtx,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Unpublish a global — sets `_status` to `"draft"` without modifying the
/// stored field data. Only available on globals with `versions.drafts`; until
/// it is published again, non-draft reads of the global return no content.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing
/// connection so an unpublish inside a hook joins the caller's transaction.
///
/// # Errors
///
/// Returns service-layer errors (access denied, hook errors) or a backend
/// error if the DB transaction or persistence fails.
#[cfg(not(tarpaulin_include))]
pub fn unpublish_global_document(ctx: &ServiceContext) -> Result<Document> {
    if ctx.pool.is_some() {
        unpublish_global_pool(ctx)
    } else {
        unpublish_global_conn(ctx)
    }
}

/// Enforce the global-update access check for an unpublish. Globals don't
/// support filter-based access, so a `Constrained` result is a config error.
fn check_unpublish_access(ctx: &ServiceContext, def: &GlobalDefinition) -> Result<()> {
    let access = ctx.write_hooks()?.check_access(
        &AccessCheckInput::builder("unpublish", ctx.slug)
            .access(def.access.update.as_ref())
            .user(ctx.user)
            .id(Some("default"))
            .ui_locale(ctx.ui_locale.as_deref())
            .build(),
    )?;

    if !global_access_allowed(&access, ctx.slug)? {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    Ok(())
}

/// Conn-mode core: everything except transaction/commit and post-commit
/// event/cache side effects. Shared by both dispatch modes. Returns the
/// stored row the unpublish event is built from alongside the document.
fn unpublish_global_in_conn(ctx: &ServiceContext) -> Result<Gated<Document>> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.global_def()?;

    // Authoritative capability gate, mirroring the collection sibling: only a
    // drafts-enabled global has a `_status` to move. Enforced here at the one
    // service chokepoint so no surface can fall through to another action.
    require_unpublish_capability(ctx)?;

    check_unpublish_access(ctx, def)?;

    let gtable = global_table(ctx.slug);

    // Lock before reading: the hooks below and the draft snapshot must see the
    // row the status write lands on, not one a concurrent publish is about to
    // replace (Postgres; no-op on SQLite).
    conn.lock_row(&gtable, "default")?;

    // Same locale-aware read fix as the collection unpublish path: when the
    // global has localized fields and locales are enabled, the fallback in
    // `get_global` emits bare column names (`title`) instead of locale-
    // suffixed ones (`title__en`), failing with `no such column`. Build a
    // default LocaleContext from the attached config to fetch all locales.
    // `get_global` reads the global with its rows for the default locale.
    let locale_ctx = ctx.default_locale_ctx();

    let mut doc = query::get_global(conn, ctx.slug, def, locale_ctx.as_ref())?;

    // Where the global sat going in: only a published one leaves the
    // published view by unpublishing.
    let prior = EventViewPlacement::from_fields(&doc.fields);

    let change = StateChange::Unpublish;
    let req_context = run_state_before_change(ctx, change, &doc, locale_ctx.as_ref())?;

    let snap_ctx = VersionSnapshotCtx::for_global(&gtable, def, ctx.locale_config);
    unpublish_with_snapshot(conn, &snap_ctx, &mut doc)?;

    run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        change.after_change(ctx, locale_ctx.as_ref(), req_context),
        conn,
    )?;

    // The global as stored, before anything is shaped or stripped for the
    // writer: the live event is built from it — and announces the now-empty
    // global to the subscribers that could only see it published.
    let row = ctx.event_row(&doc).map(|row| row.moved_from(Some(prior)));

    helpers::strip_reported(ctx, write_hooks, &mut doc, locale_ctx.as_ref())?;

    Ok((doc, row))
}

/// Conn mode (Lua CRUD): the caller owns the transaction and the event-queue
/// flush. Queue the mutation event and invalidate cache; the outer tx-commit
/// flushes the queue.
fn unpublish_global_conn(ctx: &ServiceContext) -> Result<Document> {
    let (doc, row) = unpublish_global_in_conn(ctx)?;

    ctx.clear_cache();
    ctx.publish_mutation_event(EventOperation::Unpublish, &doc.id, row);

    Ok(doc)
}

/// Pool mode: open a transaction, run the core, commit, then run the
/// post-commit side effects.
#[cfg(not(tarpaulin_include))]
fn unpublish_global_pool(ctx: &ServiceContext) -> Result<Document> {
    let (doc, _) = run_pool_write(ctx, None, unpublish_global_in_conn, |ctx, (doc, row)| {
        // Same post-commit sequence as `update_global_document` / the
        // collection unpublish path: notify subscribers of the status
        // change; the envelope flushes nested-hook events after.
        ctx.publish_mutation_event(EventOperation::Unpublish, &doc.id, row.clone());
    })?;

    Ok(doc)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use anyhow::Result as AnyResult;
    use rusqlite::Connection;
    use serde_json::{Value, json};

    use super::unpublish_global_document;
    use crate::{
        config::{CrapConfig, LocaleConfig},
        core::{
            DocumentFields, FieldDefinition, FieldType, GlobalDefinition, Hooks, Registry,
            ValidationError, VersionsConfig,
        },
        db::{
            AccessResult, DbConnection, DbPool, LocaleContext, LocaleMode, migrate, pool, query,
            query::helpers::global_table,
        },
        hooks::{AccessCheckInput, HookContext, HookEvent, ValidationCtx},
        service::{FieldReadStrip, ServiceContext, ServiceError, hooks::WriteHooks},
    };

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

    /// Regression: unpublishing a non-versioned global must fail with a typed
    /// gate error at the service chokepoint. The admin codec used to guard
    /// this itself and silently fall through to a full update instead.
    #[test]
    fn unpublish_global_rejects_non_versioned() {
        let conn = Connection::open_in_memory().unwrap();
        let def = GlobalDefinition::new("settings");

        let wh = NoopWriteHooks;
        let ctx = ServiceContext::global("settings", &def)
            .conn(&conn)
            .write_hooks(&wh)
            .build();

        let err = unpublish_global_document(&ctx).unwrap_err();
        assert!(
            matches!(&err, ServiceError::HookError(msg) if msg.contains("versioning")),
            "expected typed versioning gate error, got {err:?}"
        );
    }

    /// Store one `slides` row per locale on the `settings` global.
    fn save_slides(conn: &dyn DbConnection, def: &GlobalDefinition, locale: &LocaleConfig) {
        for (code, caption) in [("en", "english"), ("de", "deutsch")] {
            let ctx = LocaleContext {
                mode: LocaleMode::Single(code.to_string()),
                config: locale.clone(),
            };
            let data: DocumentFields = [("slides".to_string(), json!([{ "caption": caption }]))]
                .into_iter()
                .collect();

            query::save_join_table_data(
                conn,
                &global_table("settings"),
                &def.fields,
                "default",
                &data,
                Some(&ctx),
            )
            .unwrap();
        }
    }

    /// Unpublishing a global reports its rows, in the default locale.
    #[test]
    fn unpublish_reports_the_default_locales_rows() {
        let locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: false,
        };
        let mut def = GlobalDefinition::new("settings");
        def.versions = Some(VersionsConfig::new(true, 0));
        def.fields = vec![
            FieldDefinition::builder("slides", FieldType::Array)
                .localized(true)
                .fields(vec![
                    FieldDefinition::builder("caption", FieldType::Text).build(),
                ])
                .build(),
        ];

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        let shared = Registry::shared();
        shared.write().unwrap().register_global(def.clone());
        migrate::sync_all(&db_pool, &Registry::snapshot(&shared), &locale).expect("sync");

        let conn = db_pool.get().unwrap();
        save_slides(&conn, &def, &locale);

        let wh = NoopWriteHooks;
        let ctx = ServiceContext::global("settings", &def)
            .conn(&conn)
            .write_hooks(&wh)
            .locale_config(Some(&locale))
            .build();

        let doc = unpublish_global_document(&ctx).expect("unpublish");

        let captions: Vec<&str> = doc
            .fields
            .get("slides")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("caption").and_then(Value::as_str))
            .collect();
        assert_eq!(captions, vec!["english"]);
    }

    /// Records the `ctx.ui_locale` of every before/after-change hook run.
    #[derive(Default)]
    struct UiLocaleSpy {
        seen: Mutex<Vec<(HookEvent, Option<String>)>>,
    }

    impl WriteHooks for UiLocaleSpy {
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
            event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            self.seen
                .lock()
                .unwrap()
                .push((event, ctx.ui_locale.clone()));

            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _hooks: &Hooks,
            event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            self.seen
                .lock()
                .unwrap()
                .push((event, ctx.ui_locale.clone()));

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

    impl FieldReadStrip for UiLocaleSpy {}

    /// A synced drafts-enabled `banner` global with a `headline` field.
    fn drafts_global() -> (tempfile::TempDir, DbPool, GlobalDefinition) {
        let mut def = GlobalDefinition::new("banner");
        def.versions = Some(VersionsConfig::new(true, 0));
        def.fields = vec![FieldDefinition::builder("headline", FieldType::Text).build()];

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        let shared = Registry::shared();
        shared.write().unwrap().register_global(def.clone());
        migrate::sync_all(&db_pool, &Registry::snapshot(&shared), &config.locale).expect("sync");

        (tmp, db_pool, def)
    }

    /// Regression: the global unpublish hand-built its hook contexts and left
    /// out the admin UI locale the collection unpublish passes.
    #[test]
    fn unpublish_hooks_receive_the_ui_locale() {
        let (_tmp, db_pool, def) = drafts_global();
        let conn = db_pool.get().unwrap();

        let spy = UiLocaleSpy::default();
        let ctx = ServiceContext::global("banner", &def)
            .conn(&conn)
            .write_hooks(&spy)
            .ui_locale(Some("de".to_string()))
            .build();

        let doc = unpublish_global_document(&ctx).expect("unpublish");
        assert_eq!(doc.fields.get("_status"), Some(&json!("draft")));

        let seen = spy.seen.lock().unwrap();
        let events: Vec<HookEvent> = seen.iter().map(|(event, _)| *event).collect();
        assert!(events.contains(&HookEvent::BeforeChange), "{events:?}");
        assert!(events.contains(&HookEvent::AfterChange), "{events:?}");
        assert!(
            seen.iter()
                .all(|(_, locale)| locale.as_deref() == Some("de")),
            "every unpublish hook sees ctx.ui_locale, got {seen:?}"
        );
    }

    /// Unpublishing moves `_status`, which a global without drafts does not
    /// have: refused, where it used to succeed as a no-op that reported the
    /// global unpublished and recorded a spurious draft version.
    #[test]
    fn unpublish_global_rejects_versions_without_drafts() {
        let mut def = GlobalDefinition::new("settings");
        def.versions = Some(VersionsConfig::new(false, 0));

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        let shared = Registry::shared();
        shared.write().unwrap().register_global(def.clone());
        migrate::sync_all(&db_pool, &Registry::snapshot(&shared), &config.locale).expect("sync");
        let conn = db_pool.get().unwrap();

        let wh = NoopWriteHooks;
        let ctx = ServiceContext::global("settings", &def)
            .conn(&conn)
            .write_hooks(&wh)
            .build();

        let err = unpublish_global_document(&ctx).unwrap_err();
        assert!(
            matches!(&err, ServiceError::HookError(msg) if msg.contains("drafts")),
            "expected the drafts capability error, got {err:?}"
        );
        assert_eq!(
            query::count_versions(&conn, &global_table("settings"), "default", false).unwrap(),
            0,
            "no version was recorded"
        );
    }
}
