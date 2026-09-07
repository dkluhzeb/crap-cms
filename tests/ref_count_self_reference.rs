//! A document referencing itself protects nothing: deleting it removes the
//! reference too. The self-reference is never counted, so the document stays
//! deletable — and the count agrees with the back-reference list, which
//! already omits the owner.

use std::sync::Arc;

use crap_cms::config::CrapConfig;
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType, RelationshipConfig};
use crap_cms::core::{DocumentFields, Registry};
use crap_cms::db::{DbPool, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    ServiceContext, WriteInput, create_document, delete_document, update_document,
};
use serde_json::json;

fn posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("related", FieldType::Relationship)
            .relationship(RelationshipConfig::new("posts", false))
            .build(),
        FieldDefinition::builder("see_also", FieldType::Relationship)
            .relationship(RelationshipConfig::new("posts", true))
            .build(),
    ];
    def
}

fn setup() -> (tempfile::TempDir, DbPool, Arc<Registry>, HookRunner) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let shared = Registry::shared();
    shared.write().unwrap().register_collection(posts_def());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync");
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");

    (tmp, db_pool, registry, runner)
}

fn ref_count(pool: &DbPool, id: &str) -> i64 {
    let conn = pool.get().unwrap();
    query::ref_count::get_ref_count(&conn, "posts", id)
        .unwrap()
        .expect("document exists")
}

#[test]
fn a_self_reference_neither_counts_nor_blocks_deletion() {
    let (_tmp, pool, registry, runner) = setup();
    let def = registry.get_collection("posts").unwrap().clone();
    let ctx = ServiceContext::collection("posts", &def)
        .pool(&pool)
        .runner(&runner)
        .build();

    let (a, _) = create_document(
        &ctx,
        WriteInput::builder(DocumentFields::from_iter([(
            "title".to_string(),
            json!("A"),
        )]))
        .build(),
    )
    .expect("create A");
    let (b, _) = create_document(
        &ctx,
        WriteInput::builder(DocumentFields::from_iter([(
            "title".to_string(),
            json!("B"),
        )]))
        .build(),
    )
    .expect("create B");

    // A points at itself (has-one) and at itself + B (has-many).
    let data = DocumentFields::from_iter([
        ("related".to_string(), json!(a.id.to_string())),
        (
            "see_also".to_string(),
            json!([a.id.to_string(), b.id.to_string()]),
        ),
    ]);
    update_document(&ctx, &a.id, WriteInput::builder(data).build()).expect("self-reference");

    assert_eq!(ref_count(&pool, &a.id), 0, "self-references never count");
    assert_eq!(ref_count(&pool, &b.id), 1, "the reference to B counts");

    delete_document(&ctx, &a.id, None, None).expect("A is not protected by its own reference");
    assert_eq!(ref_count(&pool, &b.id), 0, "deleting A releases B");
}
