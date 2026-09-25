//! A relationship value that is not an id is rejected on write — never stored
//! as text behind the existence check and the ref count.

use std::sync::Arc;

use crap_cms::config::CrapConfig;
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType, RelationshipConfig};
use crap_cms::core::{DocumentFields, Registry};
use crap_cms::db::{DbPool, migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{ServiceContext, ServiceError, WriteInput, create_document};
use serde_json::{Value, json};

fn defs() -> Vec<CollectionDefinition> {
    let authors = CollectionDefinition::new("authors");
    let mut posts = CollectionDefinition::new("posts");
    posts.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("authors", false))
            .build(),
        FieldDefinition::builder("reviewers", FieldType::Relationship)
            .relationship(RelationshipConfig::new("authors", true))
            .build(),
    ];
    vec![authors, posts]
}

fn setup() -> (tempfile::TempDir, DbPool, Arc<Registry>, HookRunner) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for def in defs() {
            reg.register_collection(def);
        }
    }
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

fn create_post(
    pool: &DbPool,
    registry: &Arc<Registry>,
    runner: &HookRunner,
    field: &str,
    value: Value,
) -> Result<(), ServiceError> {
    let def = registry.get_collection("posts").unwrap().clone();
    let ctx = ServiceContext::collection("posts", &def)
        .pool(pool)
        .runner(runner)
        .build();
    let data = DocumentFields::from_iter([
        ("title".to_string(), json!("T")),
        (field.to_string(), value),
    ]);

    create_document(&ctx, WriteInput::builder(data).build()).map(|_| ())
}

fn assert_shape_error(result: Result<(), ServiceError>, field: &str) {
    match result {
        Err(ServiceError::Validation(ve)) => assert!(
            ve.errors.iter().any(|e| e.field == field),
            "expected a validation error on '{field}': {ve:?}"
        ),
        other => panic!("expected a validation error on '{field}', got {other:?}"),
    }
}

#[test]
fn non_id_relationship_values_are_rejected() {
    let (_tmp, pool, registry, runner) = setup();

    assert_shape_error(
        create_post(&pool, &registry, &runner, "author", json!(12345)),
        "author",
    );
    assert_shape_error(
        create_post(
            &pool,
            &registry,
            &runner,
            "author",
            json!({"id": "a1", "name": "Ann"}),
        ),
        "author",
    );
    assert_shape_error(
        create_post(&pool, &registry, &runner, "reviewers", json!([1, 2])),
        "reviewers",
    );
}

/// Regression: an empty has-many list spelled as an empty object — the shape
/// a Lua table with no entries takes — was rejected, so the list could not be
/// cleared from Lua. It writes as an empty list.
#[test]
fn an_empty_object_writes_an_empty_has_many_list() {
    let (_tmp, pool, registry, runner) = setup();

    create_post(&pool, &registry, &runner, "reviewers", json!({}))
        .expect("an empty object is an empty list");
}

/// A reference to an id that does not exist is the caller's mistake: a
/// validation error (400) on the field holding it, not an internal fault.
#[test]
fn a_reference_to_a_missing_target_is_a_caller_error() {
    let (_tmp, pool, registry, runner) = setup();

    let err = create_post(&pool, &registry, &runner, "author", json!("ghost"))
        .map_err(|e| e.reclassify("sqlite"));

    assert_shape_error(err, "author");
}
