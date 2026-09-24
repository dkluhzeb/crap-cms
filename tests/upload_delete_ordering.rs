//! a hard delete performed in **conn mode** (inside
//! an enclosing transaction — a hook, a `crap.transaction` block) must
//! NOT delete the upload's storage files immediately. Files are removed
//! only after the enclosing transaction commits, so a rollback leaves
//! orphaned files (harmless) rather than a live DB row pointing at bytes
//! that are already gone.

#![allow(clippy::missing_panics_doc, clippy::unwrap_used)]

use std::sync::Arc;

use anyhow::anyhow;
use serde_json::json;

use crap_cms::config::CrapConfig;
use crap_cms::core::collection::{CollectionDefinition, Labels};
use crap_cms::core::field::{FieldDefinition, FieldType, LocalizedString};
use crap_cms::core::upload::CollectionUpload;
use crap_cms::core::upload::StorageBackend;
use crap_cms::core::upload::storage::LocalStorage;
use crap_cms::core::{DocumentFields, Registry};
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks::lifecycle::{FileCleanupQueue, HookRunner, MigrationCall};
use crap_cms::service::{self, RunnerWriteHooks, ServiceContext};

fn media_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("media");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Media".into())),
        plural: Some(LocalizedString::Plain("Media".into())),
    };
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text).build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
    ];
    def.upload = Some(CollectionUpload::new());
    def
}

#[test]
fn conn_mode_delete_queues_file_cleanup_instead_of_deleting_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".into();

    let shared = Registry::shared();
    shared.write().unwrap().register_collection(media_def());
    let registry = Registry::snapshot(&shared);
    let pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&pool, &registry, &config.locale).unwrap();

    let storage = LocalStorage::new(tmp.path().join("uploads"));
    storage.put("media/pic.png", b"bytes", "image/png").unwrap();

    let def = registry.get_collection("media").unwrap();

    // Seed a media document referencing the stored file.
    let id = {
        let mut data = DocumentFields::new();
        data.insert("filename".into(), json!("pic.png"));
        data.insert("url".into(), json!("/uploads/media/pic.png"));
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "media", def, &data, None).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    // Conn-mode delete WITH a file_cleanup queue attached (the enclosing
    // transaction's queue). The file must survive the delete call.
    let cleanup: FileCleanupQueue = std::rc::Rc::default();
    {
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let wh = RunnerWriteHooks::new(&runner)
            .with_conn(&tx)
            .with_override_access();
        let ctx = ServiceContext::collection("media", def)
            .conn(&tx)
            .write_hooks(&wh)
            .override_access(true)
            .file_cleanup(cleanup.clone())
            .build();

        service::delete_document(&ctx, &id, Some(&storage), None).unwrap();

        // The DB row is gone within the tx, but the FILE is untouched —
        // it was queued, not deleted.
        assert!(
            storage.exists("media/pic.png").unwrap(),
            "conn-mode delete must NOT remove the file before commit"
        );
        assert_eq!(cleanup.borrow().len(), 1, "the file-map must be queued");

        // Simulate the rollback direction: drop the tx WITHOUT flushing
        // the queue → the file stays (orphaned-file-safe).
        drop(ctx);
        drop(wh);
        drop(tx);
    }
    assert!(
        storage.exists("media/pic.png").unwrap(),
        "after rollback the file must survive (orphaned file, not a dangling row)"
    );

    // Commit direction: draining the queue (now pre-resolved storage keys)
    // removes the file.
    let keys: Vec<String> = cleanup.borrow_mut().drain(..).collect();
    crap_cms::core::upload::delete_storage_keys(&storage, &keys);
    assert!(
        !storage.exists("media/pic.png").unwrap(),
        "post-commit flush must delete the file"
    );
}

/// A migrated pool with a `media` document whose file is in the local storage
/// the runner's VMs use (`<config_dir>/uploads`), plus a runner over it.
struct MediaFixture {
    tmp: tempfile::TempDir,
    pool: crap_cms::db::DbPool,
    registry: Arc<Registry>,
    runner: HookRunner,
    storage: LocalStorage,
    id: String,
}

fn media_fixture() -> MediaFixture {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".into();

    let shared = Registry::shared();
    shared.write().unwrap().register_collection(media_def());
    let registry = Registry::snapshot(&shared);
    let pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&pool, &registry, &config.locale).unwrap();

    let storage = LocalStorage::new(tmp.path().join("uploads"));
    storage.put("media/pic.png", b"bytes", "image/png").unwrap();

    let def = registry.get_collection("media").unwrap();

    let mut data = DocumentFields::new();
    data.insert("filename".into(), json!("pic.png"));
    data.insert("url".into(), json!("/uploads/media/pic.png"));

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let id = query::create(&tx, "media", def, &data, None)
        .unwrap()
        .id
        .to_string();
    tx.commit().unwrap();
    drop(conn);

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    MediaFixture {
        tmp,
        pool,
        registry,
        runner,
        storage,
        id,
    }
}

fn media_row_exists(f: &MediaFixture) -> bool {
    let def = f.registry.get_collection("media").unwrap();
    let conn = f.pool.get().unwrap();

    query::find_by_id(&conn, "media", def, &f.id, None)
        .unwrap()
        .is_some()
}

/// Write a migration whose `up` hard-deletes the media document and then runs
/// `tail` (Lua), and return its path.
fn write_delete_migration(f: &MediaFixture, tail: &str) -> std::path::PathBuf {
    let path = f.tmp.path().join("001_delete_media.lua");
    let code = format!(
        r#"
        local M = {{}}
        function M.up()
            crap.collections.delete("media", "{id}", {{ override_access = true }})
            {tail}
        end
        return M
        "#,
        id = f.id,
    );
    std::fs::write(&path, code).unwrap();

    path
}

/// Without an enclosing cleanup queue, a conn-mode delete must NOT fall back
/// to deleting the file immediately: the caller's transaction has not
/// committed, and a rollback would restore a row whose file is gone.
#[test]
fn conn_mode_delete_without_a_cleanup_queue_keeps_the_file() {
    let f = media_fixture();
    let def = f.registry.get_collection("media").unwrap();

    let mut conn = f.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    {
        let wh = RunnerWriteHooks::new(&f.runner)
            .with_conn(&tx)
            .with_override_access();
        let ctx = ServiceContext::collection("media", def)
            .conn(&tx)
            .write_hooks(&wh)
            .override_access(true)
            .build();

        service::delete_document(&ctx, &f.id, Some(&f.storage), None).unwrap();
    }
    drop(tx);

    assert!(
        f.storage.exists("media/pic.png").unwrap(),
        "a conn-mode delete with no cleanup queue must leave the file in place"
    );
    assert!(media_row_exists(&f), "the rolled-back row is back");
}

/// A migration that deletes an upload document and then fails rolls back —
/// and the file must still be there for the restored row.
#[test]
fn failing_migration_keeps_the_deleted_uploads_file() {
    let f = media_fixture();
    let path = write_delete_migration(&f, r#"error("boom")"#);

    let result =
        f.runner
            .run_migration(&MigrationCall::new(&path, "up"), &f.pool, None, |_| Ok(()));

    assert!(result.is_err(), "the migration must fail");
    assert!(media_row_exists(&f), "the delete must have rolled back");
    assert!(
        f.storage.exists("media/pic.png").unwrap(),
        "a rolled-back migration must not have removed the file"
    );
}

/// A migration's bookkeeping failing after the Lua ran rolls back the same way.
#[test]
fn migration_whose_record_step_fails_keeps_the_file() {
    let f = media_fixture();
    let path = write_delete_migration(&f, "");

    let result = f
        .runner
        .run_migration(&MigrationCall::new(&path, "up"), &f.pool, None, |_| {
            Err(anyhow!("record failed"))
        });

    let err = result.expect_err("the record step fails the migration");
    assert!(format!("{err:#}").contains("record failed"), "{err:#}");
    assert!(media_row_exists(&f), "the delete must have rolled back");
    assert!(f.storage.exists("media/pic.png").unwrap());
}

/// A successful migration removes the deleted upload's file — after the
/// commit.
#[test]
fn committed_migration_removes_the_deleted_uploads_file() {
    let f = media_fixture();
    let path = write_delete_migration(&f, "");

    f.runner
        .run_migration(&MigrationCall::new(&path, "up"), &f.pool, None, |_| Ok(()))
        .expect("migration succeeds");

    assert!(!media_row_exists(&f), "the delete committed");
    assert!(
        !f.storage.exists("media/pic.png").unwrap(),
        "the committed delete's file must be removed"
    );
}

/// `on_init` hooks run in the same scope: a failing hook rolls the delete back
/// and keeps the file.
#[test]
fn failing_system_hook_keeps_the_deleted_uploads_file() {
    let f = media_fixture();

    let hooks_dir = f.tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(
        hooks_dir.join("purge.lua"),
        format!(
            r#"
            local M = {{}}
            function M.run()
                crap.collections.delete("media", "{id}", {{ override_access = true }})
                error("boom")
            end
            return M
            "#,
            id = f.id,
        ),
    )
    .unwrap();

    let result = f
        .runner
        .run_system_hooks_in_tx(&["hooks.purge.run".to_string()], &f.pool, None);

    assert!(result.is_err(), "the hook must fail");
    assert!(media_row_exists(&f), "the delete must have rolled back");
    assert!(f.storage.exists("media/pic.png").unwrap());
}
