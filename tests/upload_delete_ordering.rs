//! Ledger class **L4**: a hard delete performed in **conn mode** (inside
//! an enclosing transaction — a hook, a `crap.transaction` block) must
//! NOT delete the upload's storage files immediately. Files are removed
//! only after the enclosing transaction commits, so a rollback leaves
//! orphaned files (harmless) rather than a live DB row pointing at bytes
//! that are already gone.

#![allow(clippy::missing_panics_doc, clippy::unwrap_used)]

use std::sync::Arc;

use serde_json::json;

use crap_cms::config::CrapConfig;
use crap_cms::core::collection::{CollectionDefinition, Labels};
use crap_cms::core::field::{FieldDefinition, FieldType, LocalizedString};
use crap_cms::core::upload::CollectionUpload;
use crap_cms::core::upload::StorageBackend;
use crap_cms::core::upload::storage::LocalStorage;
use crap_cms::core::{DocumentFields, Registry};
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks::lifecycle::{FileCleanupQueue, HookRunner};
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

    // Commit direction: draining the queue removes the file.
    for fields in cleanup.borrow_mut().drain(..) {
        crap_cms::core::upload::delete_upload_files(&storage, &fields);
    }
    assert!(
        !storage.exists("media/pic.png").unwrap(),
        "post-commit flush must delete the file"
    );
}
