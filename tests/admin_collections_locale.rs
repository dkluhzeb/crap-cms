//! Localized-collection integration tests for the admin HTTP handlers.
//!
//! Covers: listing, creating, editing, searching and deleting in a localized
//! collection, the `_locale` form parameter and locale redirects, and the
//! rejection of unknown or locked locales on create / update / validate.

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

mod admin_collections_support;

use std::collections::HashMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use crap_cms::{
    config::{CrapConfig, LocaleConfig},
    core::{
        DocumentFields,
        collection::{AdminConfig, CollectionDefinition, Labels},
        field::{FieldDefinition, FieldType, LocalizedString},
    },
    db::query,
};

use admin_collections_support::{
    TEST_CSRF, TestApp, auth_and_csrf, body_string, create_test_user, make_auth_cookie,
    make_users_def, setup_app_with_config,
};

fn make_locale_config() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

fn make_localized_pages_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Page".to_string())),
        plural: Some(LocalizedString::Plain("Pages".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .localized(true)
            .max_length(40)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea)
            .localized(true)
            .build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def
}

fn setup_localized_app() -> TestApp {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();
    setup_app_with_config(
        vec![make_localized_pages_def(), make_users_def()],
        vec![],
        config,
    )
}

/// The `_locale` input the error re-render must carry, with the value that
/// follows it in the stacked attribute list.
fn locale_input_value(body: &str) -> Option<String> {
    let after = body.split("name=\"_locale\"").nth(1)?;

    Some(after.chars().take(60).collect())
}

/// Creating a document under a NON-default locale is rejected: a new document
/// establishes its default-locale (canonical) row first — translations come
/// later via update. Creating under the default locale succeeds. (Was
/// `create_action_with_locale`, which asserted the now-retired behavior that a
/// non-default-locale create succeeds and silently wrote shared columns from the
/// wrong locale.)
#[tokio::test]
async fn create_under_non_default_locale_is_rejected() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "locale_create@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "locale_create@test.com");

    let create = |locale: &str| {
        app.router.clone().oneshot(
            Request::post("/admin/collections/pages")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(format!(
                    "title=Locale+Test+Page&body=Content+here&_locale={locale}"
                )))
                .unwrap(),
        )
    };

    // Non-default locale → rejected.
    let resp = create("de").await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "creating under a non-default locale must be rejected"
    );

    // Default locale → succeeds.
    let resp = create("en").await.unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "default-locale create should succeed, got {status}"
    );
}

/// Regression: an unknown `_locale` on a form post used to be silently
/// swallowed (`from_locale_string(...).unwrap_or(None)`) into "no locale
/// context", which on a localized collection reads/writes bare columns.
/// It must be rejected with a 422 toast naming the invalid locale.
#[tokio::test]
async fn create_with_unknown_locale_is_rejected() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "badlocale_create@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "badlocale_create@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/pages")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("title=Bad+Locale&body=Content&_locale=xx"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let toast = resp
        .headers()
        .get("X-Crap-Toast")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        toast.contains("Invalid locale"),
        "toast should name the invalid locale, got: {toast}"
    );
}

/// Regression twin of `create_with_unknown_locale_is_rejected` for the
/// update action.
#[tokio::test]
async fn update_with_unknown_locale_is_rejected() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "badlocale_update@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "badlocale_update@test.com");

    let def = app.registry.get_collection("pages").unwrap().clone();
    let locale_ctx =
        query::LocaleContext::from_locale_string(Some("en"), &make_locale_config()).unwrap();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!("Original"))]).into();
    let doc = query::create(&tx, "pages", &def, &data, locale_ctx.as_ref()).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/pages/{}", doc.id))
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("title=Updated&_locale=xx"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let toast = resp
        .headers()
        .get("X-Crap-Toast")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        toast.contains("Invalid locale"),
        "toast should name the invalid locale, got: {toast}"
    );
}

/// Regression twin for the validate endpoint: unknown locale in the JSON
/// payload must produce a validation error, not a silent bare-column run.
#[tokio::test]
async fn validate_with_unknown_locale_is_rejected() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "badlocale_validate@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "badlocale_validate@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/pages/validate")
                .header("content-type", "application/json")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    json!({ "data": { "title": "T" }, "locale": "xx" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("\"valid\":false") && body.contains("Invalid locale"),
        "validate must reject the unknown locale, got: {body}"
    );
}

#[tokio::test]
async fn localized_collection_list_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn localized_collection_list_shows_documents() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    {
        let reg = &app.registry;
        let def = reg.get_collection("pages").unwrap().clone();

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello World"));
        data.insert("body".to_string(), json!("Page body"));
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Hello World"),
        "list should contain the document title"
    );
}

#[tokio::test]
async fn localized_collection_create_via_form() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/pages")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    "title=Created+Page&body=Some+content&_locale=en",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Localized create should redirect or HX-Redirect, got {status}"
    );
}

#[tokio::test]
async fn localized_collection_edit_page_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let doc_id = {
        let reg = &app.registry;
        let def = reg.get_collection("pages").unwrap().clone();

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Editable Page"));
        data.insert("body".to_string(), json!("Content"));
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/pages/{doc_id}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Editable Page"),
        "edit page should contain the document title"
    );
}

#[tokio::test]
async fn localized_collection_delete_succeeds() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let doc_id = {
        let reg = &app.registry;
        let def = reg.get_collection("pages").unwrap().clone();

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("To Delete"));
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::delete(format!("/admin/collections/pages/{doc_id}"))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "expected redirect after delete, got {status}"
    );
}

#[tokio::test]
async fn localized_collection_search_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    {
        let reg = &app.registry;
        let def = reg.get_collection("pages").unwrap().clone();

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Searchable Page"));
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages?search=Searchable")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn update_localized_collection_redirects_with_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "updloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "updloc@test.com");

    let doc_id = {
        let reg = &app.registry;
        let def = reg.get_collection("pages").unwrap().clone();
        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Update Locale"));
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/pages/{doc_id}"))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated+Title&_locale=de"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Localized update should succeed, got {status}"
    );
    if status == StatusCode::OK
        && let Some(hx_redir) = resp.headers().get("HX-Redirect")
    {
        let redir = hx_redir.to_str().unwrap_or("");
        assert!(
            !redir.contains("locale="),
            "HX-Redirect should not contain locale= (cookie-based now), got {redir}"
        );
    }
}

#[tokio::test]
async fn create_form_with_locale_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "cfloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cfloc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages/create")
                .header("cookie", format!("{}; crap_editor_locale=de", &cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn create_form_with_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "cfloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cfloc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages/create")
                .header("cookie", format!("{}; crap_editor_locale=de", &cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("DE") || body.contains("de"),
        "Should show locale selector with DE"
    );
}

#[tokio::test]
async fn edit_form_with_non_default_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "efloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "efloc@test.com");

    let doc_id = {
        let reg = &app.registry;
        let def = reg.get_collection("pages").unwrap().clone();

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Locale Edit Test"));
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/pages/{doc_id}"))
                .header("cookie", format!("{}; crap_editor_locale=de", &cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn update_action_with_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "updloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "updloc@test.com");

    let doc_id = {
        let reg = &app.registry;
        let def = reg.get_collection("pages").unwrap().clone();

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Update Locale Test"));
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/pages/{doc_id}"))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated+DE&_locale=de".to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Update with locale should succeed, got {status}"
    );
}

/// Regression: after a validation error the re-rendered form carried no
/// `_locale` input. The corrected save then parsed no locale at all, so the
/// German text was written into the English columns and the shared fields were
/// overwritten — the publish-time strip is a no-op without a locale context.
#[tokio::test]
async fn validation_error_re_render_keeps_the_submitted_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "verrloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "verrloc@test.com");

    // A document exists in the default locale; the editor then translates it
    // and trips a field rule, so the form comes back with errors.
    let doc_id = {
        let def = app.registry.get_collection("pages").unwrap().clone();
        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Re-render locale test"));
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let too_long = "x".repeat(60);
    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/pages/{doc_id}"))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("title={too_long}&_locale=de")))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a field validation error re-renders the form"
    );

    let body = body_string(resp.into_body()).await;
    let locale_input = locale_input_value(&body).unwrap_or_else(|| {
        panic!("the re-rendered form must carry the _locale input, got: {body}")
    });

    assert!(
        locale_input.contains("value=\"de\""),
        "the form must come back in the locale it was submitted in, got: {locale_input}"
    );
}
