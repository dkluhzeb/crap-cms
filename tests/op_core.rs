//! Operation-core dispatch tests (Op Core Stage 2) — `service::op::run`
//! with the `FindById` reference operation.

use std::collections::HashMap;
use std::slice;
use std::sync::Arc;

use serde_json::json;

use crap_cms::config::CrapConfig;
use crap_cms::core::{CollectionDefinition, DocumentFields, FieldDefinition, FieldType, Registry};
use crap_cms::db::{DbPool, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::AppInfra;
use crap_cms::service::op::{self, CoreError, FindById, FindByIdArgs, Principal, TargetRef};

fn make_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
    def
}

fn setup_infra(collections: &[CollectionDefinition]) -> (tempfile::TempDir, Arc<AppInfra>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = CrapConfig::test_default();
    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for def in collections {
            reg.register_collection(def.clone());
        }
    }
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("create hook runner");
    let storage = crap_cms::core::upload::create_storage(
        tmp.path(),
        &crap_cms::config::UploadConfig::default(),
    )
    .unwrap();
    let token_provider: crap_cms::core::SharedTokenProvider = Arc::new(
        crap_cms::core::auth::JwtTokenProvider::new("test-jwt-secret"),
    );

    let infra = crap_cms::admin::test_support::test_infra(
        db_pool,
        registry,
        hook_runner,
        storage,
        token_provider,
        &config,
        tmp.path(),
    );

    (tmp, infra)
}

fn create_post(pool: &DbPool, def: &CollectionDefinition, title: &str) -> String {
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!(title))]).into();
    let doc = query::create(&tx, "posts", def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

/// The reference conversion's happy path: dispatch through `op::run` with an
/// override principal returns the document.
#[test]
fn run_find_by_id_returns_document() {
    let def = make_posts_def();
    let (_tmp, infra) = setup_infra(slice::from_ref(&def));
    let id = create_post(&infra.pool, &def, "Hello");

    let args = FindByIdArgs::builder(id.as_str()).build();
    let doc = op::run::<FindById>(
        &infra,
        Principal::Override,
        &TargetRef::collection("posts"),
        &args,
    )
    .expect("dispatch succeeds")
    .expect("document found");

    assert_eq!(doc.get_str("title"), Some("Hello"));
}

/// An unknown slug is a typed `CoreError::UnknownTarget`, resolved inside the
/// core — a codec can never smuggle a stale definition past the registry.
#[test]
fn run_unknown_collection_is_typed_error() {
    let (_tmp, infra) = setup_infra(&[make_posts_def()]);

    let args = FindByIdArgs::builder("some-id").build();
    let err = op::run::<FindById>(
        &infra,
        Principal::Override,
        &TargetRef::collection("nope"),
        &args,
    )
    .expect_err("unknown collection must error");

    assert!(
        matches!(err, CoreError::UnknownTarget { ref slug, .. } if slug == "nope"),
        "expected UnknownTarget"
    );
}

/// Regression (cross-surface harmonization): the `trash` flag on a collection
/// WITHOUT soft delete is downgraded inside the operation body — the live
/// document is returned. The Lua `find_by_id` surface used to pass the flag
/// raw, turning the request into a guaranteed miss while gRPC (and the `find`
/// list paths on every surface) downgraded; all four surfaces now share
/// `FindById::run`, so the downgrade cannot drift again.
#[test]
fn trash_flag_downgraded_on_non_soft_delete_collection() {
    let def = make_posts_def();
    assert!(!def.soft_delete);
    let (_tmp, infra) = setup_infra(slice::from_ref(&def));
    let id = create_post(&infra.pool, &def, "Live");

    let args = FindByIdArgs::builder(id.as_str())
        .include_deleted(true)
        .build();
    let doc = op::run::<FindById>(
        &infra,
        Principal::Override,
        &TargetRef::collection("posts"),
        &args,
    )
    .expect("dispatch succeeds");

    assert!(
        doc.is_some(),
        "trash flag must be ignored (not a guaranteed miss) when the collection has no soft delete"
    );
}
