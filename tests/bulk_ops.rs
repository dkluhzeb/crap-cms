//! Bulk `update_many` / `delete_many` service behavior: the
//! `server.bulk_max_documents` cap and cross-batch atomicity (a mid-operation
//! failure rolls the whole operation back, leaving no partial state).
//!
//! Drives the pool-mode service path (the one gRPC / admin / MCP use). The cap
//! and atomicity live in the service layer, so all surfaces inherit them.

#![allow(clippy::missing_panics_doc, clippy::too_many_lines)]

use std::sync::Arc;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::{CollectionDefinition, Labels};
use crap_cms::core::field::{FieldDefinition, FieldType, LocalizedString};
use crap_cms::core::{DocumentFields, Registry};
use crap_cms::db::{DbPool, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    CreateManyItem, CreateManyOptions, DeleteManyOptions, ServiceContext, ServiceError,
    UpdateManyOptions, create_many, delete_many, update_many,
};
use serde_json::json;

struct Setup {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    def: CollectionDefinition,
}

/// A `posts` collection with a **unique** `title` (used to trigger a
/// mid-operation failure for the atomicity test) and seeded with `n` docs.
fn setup(n: usize) -> Setup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .unique(true)
            .build(),
        FieldDefinition::builder("status", FieldType::Text).build(),
    ];

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def.clone());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    for i in 0..n {
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!(format!("Title {i}")));
        data.insert("status".to_string(), json!("draft"));
        let mut conn = pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        query::create(&tx, "posts", &def, &data, None).expect("create");
        tx.commit().expect("commit");
    }

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    Setup {
        _tmp: tmp,
        pool,
        runner,
        def,
    }
}

fn ctx(s: &Setup) -> ServiceContext<'_> {
    ServiceContext::collection("posts", &s.def)
        .pool(&s.pool)
        .runner(&s.runner)
        .override_access(true)
        .build()
}

fn update_opts(max_documents: i64) -> UpdateManyOptions<'static> {
    UpdateManyOptions {
        locale_ctx: None,
        run_hooks: false,
        draft: false,
        ui_locale: None,
        max_documents,
    }
}

fn count_all(s: &Setup) -> usize {
    let conn = s.pool.get().expect("conn");
    let q = query::FindQuery::builder().build();
    query::find(&conn, "posts", &s.def, &q, None)
        .expect("find")
        .len()
}

fn item(title: &str) -> CreateManyItem {
    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!(title));
    data.insert("status".to_string(), json!("draft"));
    CreateManyItem {
        data,
        password: None,
    }
}

fn count_with_status(s: &Setup, status: &str) -> usize {
    let conn = s.pool.get().expect("conn");
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::Equals(status.to_string()),
        })])
        .build();
    query::find(&conn, "posts", &s.def, &q, None)
        .expect("find")
        .len()
}

#[test]
fn update_many_over_cap_is_rejected_and_changes_nothing() {
    let s = setup(5);
    let mut data = DocumentFields::new();
    data.insert("status".to_string(), json!("published"));

    // Cap of 2, but the empty filter matches all 5.
    let err = update_many(
        &ctx(&s),
        &[],
        &data,
        &LocaleConfig::default(),
        &update_opts(2),
    )
    .expect_err("should exceed the cap");
    assert!(
        matches!(err, ServiceError::LimitExceeded(_)),
        "expected LimitExceeded, got: {err:?}"
    );

    // Nothing was changed — all 5 are still drafts.
    assert_eq!(count_with_status(&s, "published"), 0);
    assert_eq!(count_with_status(&s, "draft"), 5);
}

#[test]
fn delete_many_over_cap_is_rejected_and_changes_nothing() {
    let s = setup(5);
    let opts = DeleteManyOptions {
        run_hooks: false,
        max_documents: 2,
        ..Default::default()
    };

    let err = delete_many(&ctx(&s), &[], &LocaleConfig::default(), &opts)
        .expect_err("should exceed the cap");
    assert!(
        matches!(err, ServiceError::LimitExceeded(_)),
        "expected LimitExceeded, got: {err:?}"
    );

    assert_eq!(
        count_with_status(&s, "draft"),
        5,
        "nothing should be deleted"
    );
}

#[test]
fn update_many_unlimited_cap_updates_all() {
    let s = setup(5);
    let mut data = DocumentFields::new();
    data.insert("status".to_string(), json!("published"));

    // 0 = no limit.
    let result = update_many(
        &ctx(&s),
        &[],
        &data,
        &LocaleConfig::default(),
        &update_opts(0),
    )
    .expect("should succeed");
    assert_eq!(result.modified, 5);
    assert_eq!(count_with_status(&s, "published"), 5);
}

#[test]
fn create_many_over_cap_is_rejected_and_creates_nothing() {
    let s = setup(0);
    let items = vec![item("A"), item("B"), item("C"), item("D"), item("E")];
    let opts = CreateManyOptions {
        run_hooks: false,
        draft: false,
        max_documents: 2,
    };

    let err = create_many(&ctx(&s), &items, &opts).expect_err("should exceed the cap");
    assert!(
        matches!(err, ServiceError::LimitExceeded(_)),
        "expected LimitExceeded, got: {err:?}"
    );
    assert_eq!(count_all(&s), 0, "nothing should be created");
}

#[test]
fn create_many_is_atomic_on_mid_operation_failure() {
    let s = setup(0);
    // Third item duplicates the first's (unique) title → the create fails
    // partway. The whole batch must roll back: zero docs created.
    let items = vec![item("A"), item("B"), item("A")];
    let opts = CreateManyOptions {
        run_hooks: false,
        draft: false,
        max_documents: 0,
    };

    let err =
        create_many(&ctx(&s), &items, &opts).expect_err("duplicate title should abort the batch");
    assert!(
        !matches!(err, ServiceError::LimitExceeded(_)),
        "got: {err:?}"
    );
    assert_eq!(
        count_all(&s),
        0,
        "no document should remain after the batch rolled back"
    );
}

#[test]
fn update_many_is_atomic_on_mid_operation_failure() {
    let s = setup(5);

    // Setting every doc's (unique) title to the same value succeeds for the
    // first row and then violates the unique constraint — a failure partway
    // through the operation. The whole thing must roll back: no row keeps the
    // duplicate, and the original titles are intact.
    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("DUPLICATE"));

    let err = update_many(
        &ctx(&s),
        &[],
        &data,
        &LocaleConfig::default(),
        &update_opts(0),
    )
    .expect_err("unique violation should abort the operation");
    // Not a cap error — a real mid-op failure.
    assert!(
        !matches!(err, ServiceError::LimitExceeded(_)),
        "got: {err:?}"
    );

    // Atomicity: no row was left with the duplicate title.
    let conn = s.pool.get().expect("conn");
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Equals("DUPLICATE".to_string()),
        })])
        .build();
    let dupes = query::find(&conn, "posts", &s.def, &q, None).expect("find");
    assert_eq!(
        dupes.len(),
        0,
        "no row should have been committed with the duplicate title (operation rolled back)"
    );
}
