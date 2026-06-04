//! Regression tests for reference counting bugs.
//!
//! Each test reproduces a specific bug that was found and fixed. These tests
//! must not be removed — they prevent silent reintroduction of data corruption.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use prost_types::{ListValue, Struct, Value, value::Kind};
use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::handlers::{ContentService, ContentServiceDeps};
use crap_cms::config::*;
use crap_cms::core::Registry;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::db::{DbConnection, DbValue, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;

// ── Helpers ──────────────────────────────────────────────────────────────

fn str_val(s: &str) -> Value {
    Value {
        kind: Some(Kind::StringValue(s.to_string())),
    }
}

fn struct_val(pairs: &[(&str, Value)]) -> Value {
    let mut fields = BTreeMap::new();
    for (k, v) in pairs {
        fields.insert((*k).to_string(), v.clone());
    }
    Value {
        kind: Some(Kind::StructValue(Struct { fields })),
    }
}

fn list_val(items: Vec<Value>) -> Value {
    Value {
        kind: Some(Kind::ListValue(ListValue { values: items })),
    }
}

fn make_struct(pairs: &[(&str, &str)]) -> Struct {
    let mut fields = BTreeMap::new();
    for (k, v) in pairs {
        fields.insert(k.to_string(), str_val(v));
    }
    Struct { fields }
}

struct TestSetup {
    _tmp: tempfile::TempDir,
    service: ContentService,
    pool: crap_cms::db::DbPool,
}

fn setup(collections: Vec<CollectionDefinition>) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for def in &collections {
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

    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("email renderer"));

    let deps = ContentServiceDeps::builder()
        .pool(db_pool.clone())
        .registry(Registry::snapshot(&shared))
        .hook_runner(hook_runner)
        .config(config.clone())
        .config_dir(tmp.path().to_path_buf())
        .storage(
            crap_cms::core::upload::create_storage(
                tmp.path(),
                &crap_cms::config::UploadConfig::default(),
            )
            .unwrap(),
        )
        .email_renderer(email_renderer)
        .login_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            5, 300,
        )))
        .ip_login_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 300,
        )))
        .forgot_password_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            3, 900,
        )))
        .ip_forgot_password_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 900,
        )))
        .cache(std::sync::Arc::new(crap_cms::core::cache::NoneCache))
        .token_provider(std::sync::Arc::new(
            crap_cms::core::auth::JwtTokenProvider::new("test-jwt-secret"),
        ))
        .password_provider(std::sync::Arc::new(
            crap_cms::core::auth::Argon2PasswordProvider,
        ));

    let service = ContentService::new(deps.build());

    TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    }
}

fn make_posts_and_tags() -> Vec<CollectionDefinition> {
    let mut tags = CollectionDefinition::new("tags");
    tags.admin.use_as_title = Some("name".to_string());
    tags.fields = vec![FieldDefinition {
        name: "name".to_string(),
        field_type: FieldType::Text,
        ..Default::default()
    }];

    let mut posts = CollectionDefinition::new("posts");
    posts.admin.use_as_title = Some("title".to_string());
    posts.fields = vec![
        FieldDefinition {
            name: "title".to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        },
        FieldDefinition {
            name: "tag".to_string(),
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig::new("tags", false)),
            ..Default::default()
        },
    ];

    vec![tags, posts]
}

fn get_ref_count(setup: &TestSetup, collection: &str, id: &str) -> i64 {
    let conn = setup.pool.get().unwrap();
    query::ref_count::get_ref_count(&conn, collection, id)
        .unwrap()
        .expect("document should exist")
}

// ── Nesting matrix: relationships at every supported nesting depth ───────
//
// `_ref_count` must count a referenced document exactly once per incoming
// reference, no matter how deeply the relationship field is nested. This
// matrix exercises every supported container combination that can hold a
// relationship: top-level, Group, Array (direct + Group inside), and
// Blocks (direct + Group inside + has-many inside). A miss here means
// delete-protection silently fails for that shape.

fn rel(name: &str, has_many: bool) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Relationship)
        .relationship(RelationshipConfig::new("tags", has_many))
        .build()
}

/// `articles` collection embedding a `tags` relationship at every
/// supported nesting depth.
fn make_nesting_matrix_defs() -> Vec<CollectionDefinition> {
    let mut tags = CollectionDefinition::new("tags");
    tags.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];

    let mut articles = CollectionDefinition::new("articles");
    articles.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        // top-level has-one
        rel("tag", false),
        // Group > has-one
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![rel("seo_tag", false)])
            .build(),
        // Array > has-one (direct sub-field)
        FieldDefinition::builder("slides", FieldType::Array)
            .fields(vec![rel("slide_tag", false)])
            .build(),
        // Array > Group > has-one
        FieldDefinition::builder("variants", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("dims", FieldType::Group)
                    .fields(vec![rel("dim_tag", false)])
                    .build(),
            ])
            .build(),
        // Blocks: direct has-one, Group > has-one, and has-many
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition::new("promo", vec![rel("promo_tag", false)]),
                BlockDefinition::new(
                    "card",
                    vec![
                        FieldDefinition::builder("meta", FieldType::Group)
                            .fields(vec![rel("card_tag", false)])
                            .build(),
                    ],
                ),
                BlockDefinition::new("list", vec![rel("list_tags", true)]),
            ])
            .build(),
    ];

    vec![tags, articles]
}

async fn create_tag(setup: &TestSetup, name: &str) -> String {
    setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "tags".to_string(),
            data: Some(make_struct(&[("name", name)])),
            locale: None,
            draft: None,
        }))
        .await
        .expect("create tag")
        .into_inner()
        .document
        .expect("tag document")
        .id
}

/// Create one tag per nesting location and an `articles` document that
/// references each at its depth. Returns the article id plus labelled tag
/// ids so callers can assert `_ref_count` per location.
async fn create_matrix_article(setup: &TestSetup) -> (String, Vec<(&'static str, String)>) {
    let tags = vec![
        ("top-level", create_tag(setup, "top").await),
        ("group", create_tag(setup, "group").await),
        ("array", create_tag(setup, "array").await),
        ("array>group", create_tag(setup, "array_group").await),
        ("blocks", create_tag(setup, "block").await),
        ("blocks>group", create_tag(setup, "block_group").await),
        ("blocks>has-many", create_tag(setup, "block_hm").await),
    ];
    let id_of = |label: &str| tags.iter().find(|(l, _)| *l == label).unwrap().1.clone();

    let data = Struct {
        fields: BTreeMap::from([
            ("title".to_string(), str_val("a1")),
            ("tag".to_string(), str_val(&id_of("top-level"))),
            (
                "seo".to_string(),
                struct_val(&[("seo_tag", str_val(&id_of("group")))]),
            ),
            (
                "slides".to_string(),
                list_val(vec![struct_val(&[("slide_tag", str_val(&id_of("array")))])]),
            ),
            (
                "variants".to_string(),
                list_val(vec![struct_val(&[(
                    "dims",
                    struct_val(&[("dim_tag", str_val(&id_of("array>group")))]),
                )])]),
            ),
            (
                "content".to_string(),
                list_val(vec![
                    struct_val(&[
                        ("_block_type", str_val("promo")),
                        ("promo_tag", str_val(&id_of("blocks"))),
                    ]),
                    struct_val(&[
                        ("_block_type", str_val("card")),
                        (
                            "meta",
                            struct_val(&[("card_tag", str_val(&id_of("blocks>group")))]),
                        ),
                    ]),
                    struct_val(&[
                        ("_block_type", str_val("list")),
                        (
                            "list_tags",
                            list_val(vec![str_val(&id_of("blocks>has-many"))]),
                        ),
                    ]),
                ]),
            ),
        ]),
    };

    let article_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "articles".to_string(),
            data: Some(data),
            locale: None,
            draft: None,
        }))
        .await
        .expect("create article")
        .into_inner()
        .document
        .expect("article document")
        .id;

    (article_id, tags)
}

fn assert_all_counts(setup: &TestSetup, tags: &[(&'static str, String)], expected: i64) {
    let mut wrong = Vec::new();
    for (label, id) in tags {
        let count = get_ref_count(setup, "tags", id);
        if count != expected {
            wrong.push(format!("{label} (got {count}, want {expected})"));
        }
    }
    assert!(wrong.is_empty(), "ref_count mismatch at: {wrong:?}");
}

#[tokio::test]
async fn ref_count_counts_relationships_at_every_nesting_depth() {
    let setup = setup(make_nesting_matrix_defs());
    let (_article_id, tags) = create_matrix_article(&setup).await;

    // Every referenced tag — top-level through array>group, blocks>group,
    // and blocks>has-many — must register exactly one incoming reference.
    assert_all_counts(&setup, &tags, 1);
}

#[tokio::test]
async fn hard_delete_decrements_relationships_at_every_nesting_depth() {
    let setup = setup(make_nesting_matrix_defs());
    let (article_id, tags) = create_matrix_article(&setup).await;
    assert_all_counts(&setup, &tags, 1);

    setup
        .service
        .delete(Request::new(content::DeleteRequest {
            events: None,
            collection: "articles".to_string(),
            id: article_id,
            force_hard_delete: true,
        }))
        .await
        .expect("hard delete article");

    // Hard delete must release every nested reference back to zero.
    assert_all_counts(&setup, &tags, 0);
}

// ── Regression: UpdateMany must adjust ref counts ────────────────────────

#[tokio::test]
async fn update_many_adjusts_ref_counts() {
    let setup = setup(make_posts_and_tags());

    // Create two tags
    let tag_a_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "Tag A")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    let tag_b_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "Tag B")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    // Create a post referencing tag A
    setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "posts".into(),
            data: Some(make_struct(&[("title", "Post 1"), ("tag", &tag_a_id)])),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(get_ref_count(&setup, "tags", &tag_a_id), 1);
    assert_eq!(get_ref_count(&setup, "tags", &tag_b_id), 0);

    // UpdateMany: change all posts to reference tag B
    setup
        .service
        .update_many(Request::new(content::UpdateManyRequest {
            events: None,
            collection: "posts".into(),
            data: Some(make_struct(&[("tag", &tag_b_id)])),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(
        get_ref_count(&setup, "tags", &tag_a_id),
        0,
        "Tag A ref_count should be 0 after UpdateMany changed reference"
    );
    assert_eq!(
        get_ref_count(&setup, "tags", &tag_b_id),
        1,
        "Tag B ref_count should be 1 after UpdateMany"
    );
}

// ── Regression: DeleteMany must adjust ref counts ────────────────────────

#[tokio::test]
async fn delete_many_adjusts_ref_counts() {
    let setup = setup(make_posts_and_tags());

    let tag_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "Tag")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    // Create two posts referencing the tag
    for i in 1..=2 {
        setup
            .service
            .create(Request::new(content::CreateRequest {
                events: None,
                collection: "posts".into(),
                data: Some(make_struct(&[
                    ("title", &format!("Post {i}")),
                    ("tag", &tag_id),
                ])),
                ..Default::default()
            }))
            .await
            .unwrap();
    }

    assert_eq!(get_ref_count(&setup, "tags", &tag_id), 2);

    // DeleteMany all posts
    setup
        .service
        .delete_many(Request::new(content::DeleteManyRequest {
            events: None,
            collection: "posts".into(),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(
        get_ref_count(&setup, "tags", &tag_id),
        0,
        "Tag ref_count should be 0 after DeleteMany removed all referencing posts"
    );
}

// ── Regression: DeleteMany skips protected documents ─────────────────────

#[tokio::test]
async fn delete_many_skips_referenced_documents() {
    let setup = setup(make_posts_and_tags());

    let tag_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "Protected Tag")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    // Create a post referencing it
    setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "posts".into(),
            data: Some(make_struct(&[("title", "Post"), ("tag", &tag_id)])),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(get_ref_count(&setup, "tags", &tag_id), 1);

    // Try to DeleteMany all tags — referenced tag should be skipped
    let resp = setup
        .service
        .delete_many(Request::new(content::DeleteManyRequest {
            events: None,
            collection: "tags".into(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.deleted, 0, "Should skip tags with ref_count > 0");

    // Tag should still exist
    let found = setup
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "tags".into(),
            id: tag_id,
            ..Default::default()
        }))
        .await;
    assert!(found.is_ok(), "Protected tag should still exist");
}

// ── Regression: Version restore must adjust ref counts ───────────────────

#[tokio::test]
async fn version_restore_adjusts_ref_counts() {
    let mut collections = make_posts_and_tags();
    collections[1].versions = Some(VersionsConfig::new(true, 0));
    let setup = setup(collections);

    let tag_a_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "A")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    let tag_b_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "B")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    // Create post referencing tag A (version 1)
    let post_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "posts".into(),
            data: Some(make_struct(&[("title", "Post"), ("tag", &tag_a_id)])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    assert_eq!(get_ref_count(&setup, "tags", &tag_a_id), 1);
    assert_eq!(get_ref_count(&setup, "tags", &tag_b_id), 0);

    // Update post to reference tag B (version 2)
    setup
        .service
        .update(Request::new(content::UpdateRequest {
            events: None,
            collection: "posts".into(),
            id: post_id.clone(),
            data: Some(make_struct(&[("tag", &tag_b_id)])),
            ..Default::default()
        }))
        .await
        .unwrap();

    assert_eq!(get_ref_count(&setup, "tags", &tag_a_id), 0);
    assert_eq!(get_ref_count(&setup, "tags", &tag_b_id), 1);

    // Get version 1 (oldest — last in newest-first list)
    let versions = setup
        .service
        .list_versions(Request::new(content::ListVersionsRequest {
            collection: "posts".into(),
            id: post_id.clone(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    let v1_id = &versions.versions.last().unwrap().id;

    // Restore version 1 (which references tag A)
    setup
        .service
        .restore_version(Request::new(content::RestoreVersionRequest {
            collection: "posts".into(),
            document_id: post_id,
            version_id: v1_id.clone(),
        }))
        .await
        .unwrap();

    assert_eq!(
        get_ref_count(&setup, "tags", &tag_a_id),
        1,
        "Tag A ref_count should be 1 after restoring version that references it"
    );
    assert_eq!(
        get_ref_count(&setup, "tags", &tag_b_id),
        0,
        "Tag B ref_count should be 0 after restoring version that doesn't reference it"
    );
}

// ── Regression: creating a reference to a vanished target must fail ─────

/// If a referenced document has been hard-deleted out from under us (e.g.
/// by a concurrent process that bypassed the `ref_count` guard, or a direct
/// SQL delete), attempting to create a new document referencing it must
/// fail loudly rather than silently writing a dangling reference.
#[tokio::test]
async fn create_with_dangling_reference_fails() {
    let setup = setup(make_posts_and_tags());

    // Create a tag, then wipe it directly via SQL — bypassing the ref_count
    // check so the row is simply gone. This simulates a concurrent hard-delete
    // racing with a create.
    let tag_id = setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "tags".into(),
            data: Some(make_struct(&[("name", "Doomed")])),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap()
        .id;

    {
        let conn = setup.pool.get().unwrap();
        conn.execute(
            "DELETE FROM tags WHERE id = ?1",
            &[DbValue::Text(tag_id.clone())],
        )
        .unwrap();
    }

    // Now try to create a post referencing the vanished tag. This must fail
    // rather than silently persisting a dangling reference.
    let result = setup
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "posts".into(),
            data: Some(make_struct(&[("title", "Ghost Post"), ("tag", &tag_id)])),
            ..Default::default()
        }))
        .await;

    assert!(
        result.is_err(),
        "creating a post that references a vanished tag must fail"
    );

    // The post must not have been persisted (transaction rolled back).
    let list = setup
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".into(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        list.documents.len(),
        0,
        "no post should exist after the failed create — tx must have rolled back"
    );
}
