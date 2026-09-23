//! `trash restore` — bring a soft-deleted document back out of the trash.

use anyhow::{Context as _, Result, anyhow};

use crate::{
    core::Document,
    service::{self, AppInfra, ServiceContext, ServiceError},
};

/// Undelete through the service layer, like every other surface: the
/// collection's `before_change`/`after_change` hooks run with the operation
/// `undelete` (a refusing `before_change` leaves the document in the trash),
/// the cache is cleared, and the undelete event — and, for an auth collection,
/// the user-stream teardown — goes out on `infra`'s transports. Collection
/// access rules don't apply to the operator's CLI.
///
/// # Errors
///
/// Returns an error if the collection is unknown, the document isn't in the
/// trash, a hook refuses the undelete, or the write fails.
pub(super) fn restore_document(infra: &AppInfra, collection: &str, id: &str) -> Result<Document> {
    let def = infra
        .registry
        .get_collection(collection)
        .ok_or_else(|| anyhow!("Collection '{collection}' not found"))?;

    let ctx = ServiceContext::collection(collection, def)
        .infra(infra)
        .override_access(true)
        .build();

    service::undelete_document(&ctx, id)
        .map_err(ServiceError::into_anyhow)
        .with_context(|| format!("Failed to restore document '{id}' in '{collection}'"))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{fs, iter, path::Path, sync::Arc};

    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::CrapConfig,
        core::{
            EventReceiver, SharedEventTransport,
            event::{EventOperation, InProcessEventBus},
            upload::create_storage,
        },
        db::{DbConnection, DbValue, migrate, pool, query},
        hooks::{self, HookRunner},
        service::StandaloneInfra,
    };

    const NOTES_LUA: &str = r#"
crap.collections.define("notes", {
    soft_delete = true,
    fields = {
        { name = "title", type = "text" },
        { name = "image", type = "relationship", relationship = { collection = "media" } },
    },
    hooks = {
        before_change = { "hooks.note_hooks.before" },
        after_change = { "hooks.note_hooks.after" },
    },
})
"#;

    const MEDIA_LUA: &str = r#"
crap.collections.define("media", {
    fields = {
        { name = "alt", type = "text" },
    },
})
"#;

    const AUDIT_LUA: &str = r#"
crap.collections.define("audit", {
    fields = {
        { name = "note", type = "text" },
    },
})
"#;

    /// Each lifecycle hook logs the operation it ran for; `before_change`
    /// refuses a note titled `keep`.
    const NOTE_HOOKS_LUA: &str = r#"
local M = {}

function M.before(ctx)
    if ctx.data.title == "keep" then
        error("this note stays in the trash")
    end

    crap.collections.create("audit", { note = "before_change:" .. ctx.operation })
    return ctx
end

function M.after(ctx)
    crap.collections.create("audit", { note = "after_change:" .. ctx.operation })
    return ctx
end

return M
"#;

    /// Write the fixture project's Lua files into `dir`.
    fn write_project(dir: &Path) {
        fs::create_dir_all(dir.join("collections")).unwrap();
        fs::create_dir_all(dir.join("hooks")).unwrap();

        fs::write(dir.join("collections/notes.lua"), NOTES_LUA).unwrap();
        fs::write(dir.join("collections/media.lua"), MEDIA_LUA).unwrap();
        fs::write(dir.join("collections/audit.lua"), AUDIT_LUA).unwrap();
        fs::write(dir.join("hooks/note_hooks.lua"), NOTE_HOOKS_LUA).unwrap();
        fs::write(dir.join("init.lua"), "").unwrap();
    }

    /// The fixture project as the CLI restore sees it: an [`AppInfra`] with
    /// the Lua hooks loaded, a memory cache and an in-process event bus, and
    /// a receiver subscribed to that bus before any write.
    fn project() -> (TempDir, Arc<AppInfra>, EventReceiver) {
        let tmp = tempfile::tempdir().unwrap();
        write_project(tmp.path());

        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();

        let registry = hooks::init_lua(tmp.path(), &config).unwrap();
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
        migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();

        let hook_runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .unwrap();
        let storage = create_storage(tmp.path(), &config.upload).unwrap();

        let events: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
        let rx = events.subscribe();

        let infra = AppInfra::standalone(StandaloneInfra {
            pool: db_pool,
            registry,
            hook_runner,
            storage,
            token_provider: None,
            event_transport: Some(events),
            invalidation_transport: None,
            config: &config,
            config_dir: tmp.path(),
        })
        .unwrap();

        (tmp, infra, rx)
    }

    /// Seed a trashed note titled `title` that references media `m1`, whose
    /// reference count stands at 1.
    fn seed_trashed_note(conn: &dyn DbConnection, title: &str) {
        conn.execute("INSERT INTO media (id, _ref_count) VALUES ('m1', 1)", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO notes (id, title, image, _deleted_at) \
             VALUES ('n1', ?1, 'm1', '2026-01-01T00:00:00.000Z')",
            &[DbValue::Text(title.to_string())],
        )
        .unwrap();
    }

    /// The `audit` notes the hooks logged, in insertion order.
    fn audit_notes(conn: &dyn DbConnection) -> Vec<String> {
        conn.query_all("SELECT note FROM audit ORDER BY rowid", &[])
            .unwrap()
            .iter()
            .filter_map(|row| row.get_string("note").ok())
            .collect()
    }

    /// Whether note `n1` is still in the trash.
    fn is_trashed(conn: &dyn DbConnection) -> bool {
        conn.query_one("SELECT _deleted_at FROM notes WHERE id = 'n1'", &[])
            .unwrap()
            .and_then(|row| row.get_value(0).cloned())
            .is_some_and(|v| !matches!(v, DbValue::Null))
    }

    /// Regression: `trash restore` cleared `_deleted_at` with a raw query,
    /// skipping the collection's undelete lifecycle hooks that every other
    /// surface runs.
    #[test]
    fn restore_runs_the_undelete_lifecycle_hooks() {
        let (_tmp, infra, _rx) = project();
        let conn = infra.pool.get().unwrap();
        seed_trashed_note(&conn, "back");

        restore_document(&infra, "notes", "n1").unwrap();

        assert!(!is_trashed(&conn), "the note must leave the trash");
        assert_eq!(
            audit_notes(&conn),
            ["before_change:undelete", "after_change:undelete"]
        );
    }

    /// A `before_change` hook that refuses the undelete fails the restore and
    /// leaves the document in the trash.
    #[test]
    fn a_refusing_before_change_keeps_the_document_trashed() {
        let (_tmp, infra, _rx) = project();
        let conn = infra.pool.get().unwrap();
        seed_trashed_note(&conn, "keep");

        assert!(restore_document(&infra, "notes", "n1").is_err());

        assert!(
            is_trashed(&conn),
            "a refused restore must keep the note trashed"
        );
        assert!(
            audit_notes(&conn).is_empty(),
            "no after_change for a refused restore"
        );
    }

    /// Regression: `trash restore` published no undelete event, so a
    /// subscriber (over Redis, on `serve`) never saw the document come back.
    #[test]
    fn restore_publishes_one_undelete_event() {
        let (_tmp, infra, mut rx) = project();
        seed_trashed_note(&infra.pool.get().unwrap(), "back");

        restore_document(&infra, "notes", "n1").unwrap();

        let undeletes: Vec<_> = iter::from_fn(|| rx.try_recv().ok())
            .filter(|e| e.collection.as_ref() == "notes")
            .collect();

        assert_eq!(undeletes.len(), 1, "exactly one event for the note");
        assert!(matches!(undeletes[0].operation, EventOperation::Undelete));
        assert_eq!(undeletes[0].document_id.to_string(), "n1");
    }

    /// Regression: `trash restore` left the cache alone, so a populated read
    /// cached while the document was trashed outlived the restore.
    #[test]
    fn restore_clears_the_cache() {
        let (_tmp, infra, _rx) = project();
        seed_trashed_note(&infra.pool.get().unwrap(), "back");
        infra.cache.set("populate:notes:n1", b"stale").unwrap();

        restore_document(&infra, "notes", "n1").unwrap();

        assert!(
            !infra.cache.has("populate:notes:n1").unwrap(),
            "the restore must clear the cache"
        );
    }

    /// A soft delete never released the references the document holds, so
    /// the restore must not count them a second time.
    #[test]
    fn restore_leaves_the_referenced_ref_counts_alone() {
        let (_tmp, infra, _rx) = project();
        let conn = infra.pool.get().unwrap();
        seed_trashed_note(&conn, "back");

        restore_document(&infra, "notes", "n1").unwrap();

        let count = query::ref_count::get_ref_count(&conn, "media", "m1").unwrap();
        assert_eq!(count, Some(1));
    }

    /// A document that isn't in the trash is reported, and its hooks never
    /// run.
    #[test]
    fn restoring_a_live_document_fails() {
        let (_tmp, infra, _rx) = project();
        let conn = infra.pool.get().unwrap();
        conn.execute("INSERT INTO notes (id, title) VALUES ('n2', 'live')", &[])
            .unwrap();

        assert!(restore_document(&infra, "notes", "n2").is_err());
        assert!(
            audit_notes(&conn).is_empty(),
            "no hook runs for a live document"
        );
    }
}
