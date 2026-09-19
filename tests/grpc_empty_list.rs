//! An empty has-many list sent over gRPC clears the list even when a
//! `before_validate` hook rebuilds the data from Lua — where an empty list
//! can only come back as an empty table, which crosses the boundary as an
//! empty object.

#![allow(clippy::missing_panics_doc, clippy::too_many_lines)]

use std::{collections::HashMap, sync::Arc};

use crap_cms::{
    api::{
        content,
        content::content_api_server::ContentApi,
        handlers::{ContentService, ContentServiceDeps},
    },
    config::{CrapConfig, UploadConfig},
    core::{
        HookRef, Registry,
        auth::{Argon2PasswordProvider, JwtTokenProvider},
        cache::NoneCache,
        collection::CollectionDefinition,
        email::EmailRenderer,
        field::{FieldDefinition, FieldType, RelationshipConfig},
        rate_limit::LoginRateLimiter,
        upload::create_storage,
    },
    db::{migrate, pool},
    hooks::lifecycle::HookRunner,
};
use tonic::Request;

/// The hook every write of `posts` runs: it changes nothing, but the data it
/// hands back has crossed the Lua boundary.
const PASS_HOOK: &str = r"
local M = {}
function M.pass(ctx)
    return ctx
end
return M
";

fn str_val(s: &str) -> content::FieldValue {
    content::FieldValue {
        kind: Some(content::field_value::Kind::StringValue(s.to_string())),
    }
}

fn list_val(items: Vec<content::FieldValue>) -> content::FieldValue {
    content::FieldValue {
        kind: Some(content::field_value::Kind::ListValue(content::FieldList {
            values: items,
        })),
    }
}

fn data_map(fields: Vec<(&str, content::FieldValue)>) -> content::DataMap {
    content::DataMap {
        fields: fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<HashMap<_, _>>(),
    }
}

fn list_items(doc: &content::Document, field: &str) -> Vec<content::FieldValue> {
    match doc.fields.as_ref().and_then(|s| s.fields.get(field)) {
        Some(content::FieldValue {
            kind: Some(content::field_value::Kind::ListValue(lv)),
        }) => lv.values.clone(),
        _ => Vec::new(),
    }
}

fn defs() -> Vec<CollectionDefinition> {
    let mut tags = CollectionDefinition::new("tags");
    tags.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];

    let mut posts = CollectionDefinition::new("posts");
    posts.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build(),
        FieldDefinition::builder("keywords", FieldType::Text)
            .has_many(true)
            .build(),
    ];
    posts.hooks.before_validate = vec![HookRef::new("hooks.post_hooks.pass")];

    vec![tags, posts]
}

struct TestSetup {
    _tmp: tempfile::TempDir,
    service: ContentService,
}

fn setup() -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(hooks_dir.join("post_hooks.lua"), PASS_HOOK).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let shared = Registry::shared();
    for def in defs() {
        shared.write().unwrap().register_collection(def);
    }
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");
    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("email renderer"));

    let service = ContentService::new(
        ContentServiceDeps::builder()
            .pool(db_pool)
            .registry(registry)
            .hook_runner(hook_runner)
            .config(config)
            .config_dir(tmp.path().to_path_buf())
            .storage(create_storage(tmp.path(), &UploadConfig::default()).unwrap())
            .email_renderer(email_renderer)
            .login_limiter(Arc::new(LoginRateLimiter::new(5, 300)))
            .ip_login_limiter(Arc::new(LoginRateLimiter::new(20, 300)))
            .forgot_password_limiter(Arc::new(LoginRateLimiter::new(3, 900)))
            .ip_forgot_password_limiter(Arc::new(LoginRateLimiter::new(20, 900)))
            .cache(Arc::new(NoneCache))
            .token_provider(Arc::new(JwtTokenProvider::new("test-jwt-secret")))
            .password_provider(Arc::new(Argon2PasswordProvider))
            .build(),
    );

    TestSetup { _tmp: tmp, service }
}

async fn create(ts: &TestSetup, collection: &str, data: content::DataMap) -> content::Document {
    ts.service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: collection.to_string(),
            data: Some(data),
            locale: None,
            draft: None,
        }))
        .await
        .expect("create")
        .into_inner()
        .document
        .expect("document")
}

async fn find(ts: &TestSetup, id: &str) -> content::Document {
    ts.service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: id.to_string(),
            depth: Some(0),
            ..Default::default()
        }))
        .await
        .expect("find")
        .into_inner()
        .document
        .expect("document")
}

/// Regression: `tags: []` on a collection with a `before_validate` hook was
/// rejected — the hook handed the data back from Lua, where the empty list had
/// become an empty table, and validation read the resulting `{}` as "not a
/// list". The list is cleared, a reference list and a scalar list alike.
#[tokio::test]
async fn an_empty_list_clears_a_has_many_field_through_a_before_validate_hook() {
    let ts = setup();

    let tag = create(&ts, "tags", data_map(vec![("name", str_val("rust"))])).await;
    let post = create(
        &ts,
        "posts",
        data_map(vec![
            ("title", str_val("Tagged")),
            ("tags", list_val(vec![str_val(&tag.id)])),
            ("keywords", list_val(vec![str_val("a"), str_val("b")])),
        ]),
    )
    .await;

    let before = find(&ts, &post.id).await;
    assert_eq!(list_items(&before, "tags").len(), 1);
    assert_eq!(list_items(&before, "keywords").len(), 2);

    ts.service
        .update(Request::new(content::UpdateRequest {
            events: None,
            collection: "posts".to_string(),
            id: post.id.clone(),
            data: Some(data_map(vec![
                ("tags", list_val(Vec::new())),
                ("keywords", list_val(Vec::new())),
            ])),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .expect("an empty list clears the field");

    let after = find(&ts, &post.id).await;
    assert!(list_items(&after, "tags").is_empty(), "{after:?}");
    assert!(list_items(&after, "keywords").is_empty(), "{after:?}");
}
