use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::query::LocaleContext;

use crate::helpers::*;
use crate::html;

// ── Definition builders ──────────────────────────────────────────────────

fn make_localized_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .localized(true)
            .build(),
        FieldDefinition::builder("slug", FieldType::Text)
            .required(true)
            .localized(false)
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

fn make_locale_config() -> CrapConfig {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.locale = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    config
}

// ── Local helpers ────────────────────────────────────────────────────────

async fn get_create_form(app: &TestApp, slug: &str, cookie: &str) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/{slug}/create"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_string(resp.into_body()).await
}

async fn get_create_form_with_locale(
    app: &TestApp,
    slug: &str,
    cookie: &str,
    locale: &str,
) -> String {
    let cookie_header = format!("{}; crap_editor_locale={}", cookie, locale);
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/{slug}/create"))
                .header("cookie", &cookie_header)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_string(resp.into_body()).await
}

async fn get_edit_form_with_locale(
    app: &TestApp,
    slug: &str,
    id: &str,
    cookie: &str,
    locale: &str,
) -> String {
    let cookie_header = format!("{}; crap_editor_locale={}", cookie, locale);
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/{slug}/{id}"))
                .header("cookie", &cookie_header)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_string(resp.into_body()).await
}

// ── Tests ────────────────────────────────────────────────────────────────

// 1. Locale enabled shows locale picker
#[tokio::test]
async fn locale_enabled_shows_locale_picker() {
    let config = make_locale_config();
    let app = setup_app_with_config(vec![make_localized_def(), make_users_def()], vec![], config);
    let user_id = create_test_user(&app, "loc1@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "loc1@test.com");

    let body = get_create_form(&app, "articles", &cookie).await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        "crap-locale-picker",
        "locale picker element should exist",
    );
    assert!(
        body.contains("locale-picker__badge"),
        "locale picker badge should be present"
    );
}

// 2. Locale disabled means no picker
#[tokio::test]
async fn locale_disabled_no_picker() {
    // Default config has empty locales
    let app = setup_app(vec![make_localized_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "loc2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "loc2@test.com");

    let body = get_create_form(&app, "articles", &cookie).await;
    let doc = html::parse(&body);

    html::assert_not_exists(
        &doc,
        "crap-locale-picker",
        "locale picker should not exist when locales disabled",
    );
}

// 3. Default locale fields not locked
#[tokio::test]
async fn default_locale_fields_not_locked() {
    let config = make_locale_config();
    let app = setup_app_with_config(vec![make_localized_def(), make_users_def()], vec![], config);
    let user_id = create_test_user(&app, "loc3@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "loc3@test.com");

    let body = get_create_form_with_locale(&app, "articles", &cookie, "en").await;
    let doc = html::parse(&body);

    // In default locale, no fields should be readonly due to locale locking
    html::assert_not_exists(
        &doc,
        "input[name=\"title\"][readonly]",
        "title should not be readonly in default locale",
    );
    html::assert_not_exists(
        &doc,
        "input[name=\"slug\"][readonly]",
        "slug should not be readonly in default locale",
    );
}

// 4. Non-default locale locks non-localized fields
#[tokio::test]
async fn non_default_locale_non_localized_field_locked() {
    let config = make_locale_config();
    let app = setup_app_with_config(vec![make_localized_def(), make_users_def()], vec![], config);
    let user_id = create_test_user(&app, "loc4@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "loc4@test.com");

    let body = get_create_form_with_locale(&app, "articles", &cookie, "de").await;
    let doc = html::parse(&body);

    // slug is not localized, should be readonly in non-default locale
    html::assert_exists(
        &doc,
        "input[name=\"slug\"][readonly]",
        "slug should be readonly in non-default locale",
    );
}

// 5. Non-default locale keeps localized fields editable
#[tokio::test]
async fn non_default_locale_localized_field_editable() {
    let config = make_locale_config();
    let app = setup_app_with_config(vec![make_localized_def(), make_users_def()], vec![], config);
    let user_id = create_test_user(&app, "loc5@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "loc5@test.com");

    let body = get_create_form_with_locale(&app, "articles", &cookie, "de").await;
    let doc = html::parse(&body);

    // title is localized, should NOT be readonly even in non-default locale
    html::assert_not_exists(
        &doc,
        "input[name=\"title\"][readonly]",
        "title (localized) should not be readonly in non-default locale",
    );
}

// 6. Form includes locale hidden field
#[tokio::test]
async fn form_includes_locale_hidden_field() {
    let config = make_locale_config();
    let app = setup_app_with_config(vec![make_localized_def(), make_users_def()], vec![], config);
    let user_id = create_test_user(&app, "loc6@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "loc6@test.com");

    let body = get_create_form_with_locale(&app, "articles", &cookie, "de").await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        "input[name=\"_locale\"][type=\"hidden\"]",
        "_locale hidden field should exist",
    );
    html::assert_input(&doc, "_locale", "hidden", Some("de"));
}

// 7. Create in default locale roundtrip
#[tokio::test]
async fn create_in_default_locale_roundtrip() {
    let config = make_locale_config();
    let app = setup_app_with_config(vec![make_localized_def(), make_users_def()], vec![], config);
    let user_id = create_test_user(&app, "loc7@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "loc7@test.com");

    // Create with default locale
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/articles")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("_locale=en&title=Hello&slug=hello&body=World"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "create should succeed, got {status}"
    );

    // Find the doc from the list
    let list_resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/articles")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = body_string(list_resp.into_body()).await;
    let list_doc = html::parse(&list_body);
    let edit_link = html::select_one(&list_doc, "table tbody tr a[href]");
    let href = edit_link.value().attr("href").unwrap();
    let doc_id = href.rsplit('/').next().unwrap();

    // GET edit form in en locale
    let body = get_edit_form_with_locale(&app, "articles", doc_id, &cookie, "en").await;
    let doc = html::parse(&body);

    html::assert_input(&doc, "title", "text", Some("Hello"));
}

// 8. Edit form in non-default locale shows localized values
#[tokio::test]
async fn edit_in_non_default_locale_shows_localized_values() {
    let config = make_locale_config();
    let app = setup_app_with_config(vec![make_localized_def(), make_users_def()], vec![], config);
    let user_id = create_test_user(&app, "loc8@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "loc8@test.com");

    // Create a doc in the default locale (en)
    let locale_config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let en_locale_ctx = LocaleContext::from_locale_string(Some("en"), &locale_config).unwrap();
    let data = std::collections::HashMap::from([
        ("title".to_string(), "Hello".to_string()),
        ("slug".to_string(), "hello".to_string()),
        ("body".to_string(), "World".to_string()),
    ]);
    let doc_record =
        crap_cms::db::query::create(&tx, "articles", &def, &data, en_locale_ctx.as_ref()).unwrap();
    // Write de locale values
    let de_locale_ctx = LocaleContext::from_locale_string(Some("de"), &locale_config).unwrap();
    let de_data = std::collections::HashMap::from([
        ("title".to_string(), "Hallo".to_string()),
        ("body".to_string(), "Welt".to_string()),
    ]);
    crap_cms::db::query::update(
        &tx,
        "articles",
        &def,
        &doc_record.id,
        &de_data,
        de_locale_ctx.as_ref(),
    )
    .unwrap();
    tx.commit().unwrap();

    // GET edit form in de locale
    let body = get_edit_form_with_locale(&app, "articles", &doc_record.id, &cookie, "de").await;
    let doc = html::parse(&body);

    // Localized title should show the de value
    html::assert_input(&doc, "title", "text", Some("Hallo"));
    // Non-localized slug should still show the original value (and be readonly)
    html::assert_input(&doc, "slug", "text", Some("hello"));
    html::assert_exists(
        &doc,
        "input[name=\"slug\"][readonly]",
        "slug should be readonly in non-default locale",
    );
}
