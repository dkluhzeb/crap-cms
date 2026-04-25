use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::db::query::LocaleContext;

use crate::helpers::*;
use crate::html;
use serde_json::json;

// ── Definition builders ──────────────────────────────────────────────────

fn make_blocks_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Page".to_string())),
        plural: Some(LocalizedString::Plain("Pages".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition {
                    block_type: "text_block".to_string(),
                    fields: vec![FieldDefinition::builder("body", FieldType::Textarea).build()],
                    label: Some(LocalizedString::Plain("Text Block".to_string())),
                    ..Default::default()
                },
                BlockDefinition {
                    block_type: "image_block".to_string(),
                    fields: vec![
                        FieldDefinition::builder("url", FieldType::Text)
                            .required(true)
                            .build(),
                        FieldDefinition::builder("alt", FieldType::Text).build(),
                    ],
                    label: Some(LocalizedString::Plain("Image Block".to_string())),
                    ..Default::default()
                },
            ])
            .build(),
    ];
    def
}

fn make_group_group_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("companies");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Company".to_string())),
        plural: Some(LocalizedString::Plain("Companies".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("address", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("geo", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("lat", FieldType::Text).build(),
                        FieldDefinition::builder("lng", FieldType::Text).build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("name".to_string()),
        ..AdminConfig::default()
    };
    def
}

fn make_array_collapsible_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("faqs");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("FAQ".to_string())),
        plural: Some(LocalizedString::Plain("FAQs".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("details", FieldType::Collapsible)
                    .fields(vec![
                        FieldDefinition::builder("question", FieldType::Text)
                            .required(true)
                            .build(),
                        FieldDefinition::builder("answer", FieldType::Textarea).build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
}

fn make_array_row_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("schedules");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Schedule".to_string())),
        plural: Some(LocalizedString::Plain("Schedules".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("slots", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("time_range", FieldType::Row)
                    .fields(vec![
                        FieldDefinition::builder("start", FieldType::Text)
                            .required(true)
                            .build(),
                        FieldDefinition::builder("end", FieldType::Text).build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
}

fn make_triple_nesting_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("deep");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Deep".to_string())),
        plural: Some(LocalizedString::Plain("Deeps".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("info", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("meta", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("value", FieldType::Text)
                                    .required(true)
                                    .build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
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

async fn post_create(app: &TestApp, slug: &str, cookie: &str, form_body: &str) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/{slug}"))
                .header("cookie", auth_and_csrf(cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "validation error should re-render form (200), got {status}"
    );
    body_string(resp.into_body()).await
}

// ── Tests ────────────────────────────────────────────────────────────────

// 1. Blocks field renders with block type templates
#[tokio::test]
async fn create_form_blocks_renders() {
    let app = setup_app(vec![make_blocks_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "blocks@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "blocks@test.com");

    let body = get_create_form(&app, "pages", &cookie).await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        "[data-field-type=\"blocks\"]",
        "blocks field type marker",
    );
    assert!(
        body.contains("text_block") && body.contains("image_block"),
        "body should contain both block type names"
    );
}

// 2. Blocks validation error on required sub-field
#[tokio::test]
async fn blocks_validation_error_on_required_sub_field() {
    let app = setup_app(vec![make_blocks_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "blkval@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "blkval@test.com");

    let body = post_create(
        &app,
        "pages",
        &cookie,
        "title=Test+Page&content[0][_block_type]=image_block&content[0][url]=&content[0][alt]=photo",
    )
    .await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        ".form__array-row .form__error",
        "blocks sub-field should have validation error",
    );
}

// 3. Group > Group renders nested fieldsets
#[tokio::test]
async fn create_form_group_group_renders() {
    let app = setup_app(vec![make_group_group_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "gg@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gg@test.com");

    let body = get_create_form(&app, "companies", &cookie).await;
    let doc = html::parse(&body);

    // Nested group fieldsets
    let groups = html::count(&doc, "fieldset.form__group");
    assert!(
        groups >= 2,
        "should have at least 2 nested group fieldsets, got {groups}"
    );

    // Nested group sub-fields use double-underscore naming (matches DB columns)
    assert!(
        body.contains("address__geo__lat"),
        "nested group sub-field lat should use __ naming"
    );
    assert!(
        body.contains("address__geo__lng"),
        "nested group sub-field lng should use __ naming"
    );
}

// 4. Group > Group CRUD roundtrip — create with flat columns, verify edit form shows values
#[tokio::test]
async fn group_group_crud_roundtrip() {
    let app = setup_app(vec![make_group_group_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ggcrud@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ggcrud@test.com");

    // Insert a doc with nested group data via flat column names
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("companies").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("name".to_string(), "Corp".to_string()),
        ("address__geo__lat".to_string(), "40.7128".to_string()),
        ("address__geo__lng".to_string(), "-74.0060".to_string()),
    ]);
    let doc_record = crap_cms::db::query::create(&tx, "companies", &def, &data, None).unwrap();
    tx.commit().unwrap();

    // GET edit form and verify nested group values are populated
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/companies/{}", doc_record.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    html::assert_input(&doc, "name", "text", Some("Corp"));
    // Verify nested group inputs have their values populated (full roundtrip)
    html::assert_input(&doc, "address__geo__lat", "text", Some("40.7128"));
    html::assert_input(&doc, "address__geo__lng", "text", Some("-74.0060"));
}

// 5. Array > Collapsible renders
#[tokio::test]
async fn create_form_array_collapsible_renders() {
    let app = setup_app(vec![make_array_collapsible_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "acol@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "acol@test.com");

    let body = get_create_form(&app, "faqs", &cookie).await;
    let doc = html::parse(&body);

    html::assert_field_exists(&doc, "items");
    html::assert_exists(
        &doc,
        "[data-field-type=\"array\"]",
        "array field type marker",
    );

    // Template should exist with collapsible sub-field names
    html::assert_exists(&doc, "template", "array template for new rows");
    assert!(
        body.contains("question") && body.contains("answer"),
        "template should contain collapsible sub-field names"
    );
}

// 6. Array > Collapsible validation error
#[tokio::test]
async fn array_collapsible_validation_error() {
    let app = setup_app(vec![make_array_collapsible_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "acolval@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "acolval@test.com");

    let body = post_create(
        &app,
        "faqs",
        &cookie,
        "title=FAQ&items[0][question]=&items[0][answer]=Some+answer",
    )
    .await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        ".form__array-row .form__error",
        "collapsible sub-field should have validation error",
    );
}

// 7. Array > Row renders
#[tokio::test]
async fn create_form_array_row_renders() {
    let app = setup_app(vec![make_array_row_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "arow@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "arow@test.com");

    let body = get_create_form(&app, "schedules", &cookie).await;
    let doc = html::parse(&body);

    html::assert_field_exists(&doc, "slots");
    html::assert_exists(&doc, "template", "array template for new rows");

    // Template should contain row sub-field names
    assert!(
        body.contains("start") && body.contains("end"),
        "template should contain row sub-field names"
    );
}

// 8. Array > Row validation error
#[tokio::test]
async fn array_row_validation_error() {
    let app = setup_app(vec![make_array_row_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "arowval@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "arowval@test.com");

    let body = post_create(
        &app,
        "schedules",
        &cookie,
        "name=Sched&slots[0][start]=&slots[0][end]=17:00",
    )
    .await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        ".form__array-row .form__error",
        "row sub-field should have validation error",
    );
}

// 9. Array > Group > Group renders all levels (triple nesting)
#[tokio::test]
async fn create_form_triple_nesting_renders() {
    let app = setup_app(vec![make_triple_nesting_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "triple@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "triple@test.com");

    let body = get_create_form(&app, "deep", &cookie).await;
    let doc = html::parse(&body);

    html::assert_field_exists(&doc, "items");
    html::assert_exists(
        &doc,
        "[data-field-type=\"array\"]",
        "array field type marker",
    );

    // Template should exist with deeply nested field names
    html::assert_exists(&doc, "template", "array template");
    assert!(
        body.contains("value"),
        "template should contain deeply nested field name"
    );
}

// 10. Triple nesting validation error
#[tokio::test]
async fn triple_nesting_validation_error() {
    let app = setup_app(vec![make_triple_nesting_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "tripleval@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "tripleval@test.com");

    let body = post_create(
        &app,
        "deep",
        &cookie,
        "name=Test&items[0][info][0][meta][0][value]=",
    )
    .await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        ".form__array-row .form__error",
        "deeply nested sub-field should have validation error",
    );
}

// ── 5-level deep nesting ─────────────────────────────────────────────────

/// 5 levels of nested groups with mixed field types at the deepest level.
fn make_five_level_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("orgs");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Org".to_string())),
        plural: Some(LocalizedString::Plain("Orgs".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("org", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("dept", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("team", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("lead", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("contact", FieldType::Group)
                                            .fields(vec![
                                                FieldDefinition::builder("phone", FieldType::Text)
                                                    .build(),
                                                FieldDefinition::builder("rank", FieldType::Number)
                                                    .build(),
                                            ])
                                            .build(),
                                    ])
                                    .build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("name".to_string()),
        ..AdminConfig::default()
    };
    def
}

// 11. Five-level deep group nesting renders correct __ names
#[tokio::test]
async fn five_level_group_nesting_renders() {
    let app = setup_app(vec![make_five_level_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "deep5@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "deep5@test.com");

    let body = get_create_form(&app, "orgs", &cookie).await;
    let doc = html::parse(&body);

    // 6 nested group fieldsets (org, dept, team, lead, contact) — at least 5
    let groups = html::count(&doc, "fieldset.form__group");
    assert!(
        groups >= 5,
        "should have at least 5 nested group fieldsets, got {groups}"
    );

    // All leaf fields use full __ chain
    assert!(
        body.contains("org__dept__team__lead__contact__phone"),
        "5-level nested field 'phone' should use full __ naming"
    );
    assert!(
        body.contains("org__dept__team__lead__contact__rank"),
        "5-level nested field 'rank' should use full __ naming"
    );
}

// 12. Five-level deep group CRUD roundtrip
#[tokio::test]
async fn five_level_group_crud_roundtrip() {
    let app = setup_app(vec![make_five_level_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "deep5rt@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "deep5rt@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("orgs").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("name".to_string(), "Acme".to_string()),
        (
            "org__dept__team__lead__contact__phone".to_string(),
            "+1-555-0100".to_string(),
        ),
        (
            "org__dept__team__lead__contact__rank".to_string(),
            "42".to_string(),
        ),
    ]);
    let doc_record = crap_cms::db::query::create(&tx, "orgs", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/orgs/{}", doc_record.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let doc = html::parse(&body);

    html::assert_input(&doc, "name", "text", Some("Acme"));
    html::assert_input(
        &doc,
        "org__dept__team__lead__contact__phone",
        "text",
        Some("+1-555-0100"),
    );
    html::assert_input(
        &doc,
        "org__dept__team__lead__contact__rank",
        "number",
        Some("42.0"),
    );
}

// ── Nested groups with locales ───────────────────────────────────────────

/// Nested groups with locale support: localized group (details) vs non-localized group (meta).
///
/// Leaf fields inside the localized group are individually marked `localized: true` to ensure
/// the write path applies locale suffixes (locale_write_column checks the leaf's flag).
fn make_locale_nesting_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("products");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Product".to_string())),
        plural: Some(LocalizedString::Plain("Products".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        // Localized group: all children editable in non-default locale
        FieldDefinition::builder("details", FieldType::Group)
            .localized(true)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text)
                    .localized(true)
                    .build(),
                FieldDefinition::builder("info", FieldType::Group)
                    .localized(true)
                    .fields(vec![
                        FieldDefinition::builder("tagline", FieldType::Text)
                            .localized(true)
                            .build(),
                        FieldDefinition::builder("notes", FieldType::Textarea)
                            .localized(true)
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
        // Non-localized group: all children locked in non-default locale
        FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("sku", FieldType::Text).build(),
                FieldDefinition::builder("weight", FieldType::Number).build(),
            ])
            .build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("name".to_string()),
        ..AdminConfig::default()
    };
    def
}

fn make_locale_nesting_config() -> CrapConfig {
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

// 13. Nested groups + locales: default locale renders all fields editable with __ naming
#[tokio::test]
async fn nested_groups_locale_default_all_editable() {
    let config = make_locale_nesting_config();
    let app = setup_app_with_config(
        vec![make_locale_nesting_def(), make_users_def()],
        vec![],
        config,
    );
    let user_id = create_test_user(&app, "nloc1@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nloc1@test.com");

    let body = get_create_form_with_locale(&app, "products", &cookie, "en").await;
    let doc = html::parse(&body);

    // Correct __ naming for nested group fields
    assert!(
        body.contains("details__title"),
        "localized group field should use __ naming"
    );
    assert!(
        body.contains("details__info__tagline"),
        "deeply nested localized field should use __ naming"
    );
    assert!(
        body.contains("meta__sku"),
        "non-localized group field should use __ naming"
    );

    // In default locale, nothing should be readonly
    html::assert_not_exists(
        &doc,
        "input[name=\"details__title\"][readonly]",
        "details__title should not be readonly in default locale",
    );
    html::assert_not_exists(
        &doc,
        "input[name=\"details__info__tagline\"][readonly]",
        "details__info__tagline should not be readonly in default locale",
    );
    html::assert_not_exists(
        &doc,
        "input[name=\"meta__sku\"][readonly]",
        "meta__sku should not be readonly in default locale",
    );
}

// 14. Nested groups + locales: non-default locale locks non-localized, keeps localized editable
#[tokio::test]
async fn nested_groups_locale_non_default_locking() {
    let config = make_locale_nesting_config();
    let app = setup_app_with_config(
        vec![make_locale_nesting_def(), make_users_def()],
        vec![],
        config,
    );
    let user_id = create_test_user(&app, "nloc2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nloc2@test.com");

    let body = get_create_form_with_locale(&app, "products", &cookie, "de").await;
    let doc = html::parse(&body);

    // Localized group children should remain editable in non-default locale
    html::assert_not_exists(
        &doc,
        "input[name=\"details__title\"][readonly]",
        "localized details__title should not be readonly in DE",
    );
    html::assert_not_exists(
        &doc,
        "input[name=\"details__info__tagline\"][readonly]",
        "localized details__info__tagline should not be readonly in DE",
    );

    // Non-localized group children should be locked in non-default locale
    html::assert_exists(
        &doc,
        "input[name=\"meta__sku\"][readonly]",
        "non-localized meta__sku should be readonly in DE",
    );
    html::assert_exists(
        &doc,
        "input[name=\"meta__weight\"][readonly]",
        "non-localized meta__weight should be readonly in DE",
    );
}

// 15. Nested groups + locales: full CRUD roundtrip across two locales
#[tokio::test]
async fn nested_groups_locale_roundtrip() {
    let config = make_locale_nesting_config();
    let app = setup_app_with_config(
        vec![make_locale_nesting_def(), make_users_def()],
        vec![],
        config,
    );
    let user_id = create_test_user(&app, "nloc3@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nloc3@test.com");

    let locale_config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("products").unwrap().clone()
    };

    // Create document in EN locale
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let en_locale_ctx = LocaleContext::from_locale_string(Some("en"), &locale_config).unwrap();
    let en_data = std::collections::HashMap::from([
        ("name".to_string(), "Widget".to_string()),
        ("details__title".to_string(), "Great Widget".to_string()),
        (
            "details__info__tagline".to_string(),
            "Best in class".to_string(),
        ),
        (
            "details__info__notes".to_string(),
            "EN notes here".to_string(),
        ),
        ("meta__sku".to_string(), "WDG-001".to_string()),
        ("meta__weight".to_string(), "250".to_string()),
    ]);
    let doc_record =
        crap_cms::db::query::create(&tx, "products", &def, &en_data, en_locale_ctx.as_ref())
            .unwrap();

    // Update DE locale translations for localized fields
    let de_locale_ctx = LocaleContext::from_locale_string(Some("de"), &locale_config).unwrap();
    let de_data = std::collections::HashMap::from([
        ("details__title".to_string(), "Tolles Widget".to_string()),
        (
            "details__info__tagline".to_string(),
            "Beste Qualität".to_string(),
        ),
        (
            "details__info__notes".to_string(),
            "DE Notizen hier".to_string(),
        ),
    ]);
    crap_cms::db::query::update(
        &tx,
        "products",
        &def,
        &doc_record.id,
        &de_data,
        de_locale_ctx.as_ref(),
    )
    .unwrap();
    tx.commit().unwrap();

    // Verify EN edit form shows EN values
    let en_body = get_edit_form_with_locale(&app, "products", &doc_record.id, &cookie, "en").await;
    let en_doc = html::parse(&en_body);

    html::assert_input(&en_doc, "name", "text", Some("Widget"));
    html::assert_input(&en_doc, "details__title", "text", Some("Great Widget"));
    html::assert_input(
        &en_doc,
        "details__info__tagline",
        "text",
        Some("Best in class"),
    );
    html::assert_input(&en_doc, "meta__sku", "text", Some("WDG-001"));
    html::assert_input(&en_doc, "meta__weight", "number", Some("250.0"));

    // Verify DE edit form shows DE values for localized fields, EN for non-localized
    let de_body = get_edit_form_with_locale(&app, "products", &doc_record.id, &cookie, "de").await;
    let de_doc = html::parse(&de_body);

    html::assert_input(&de_doc, "name", "text", Some("Widget"));
    html::assert_input(&de_doc, "details__title", "text", Some("Tolles Widget"));
    html::assert_input(
        &de_doc,
        "details__info__tagline",
        "text",
        Some("Beste Qualität"),
    );
    // Non-localized fields show the same value regardless of locale
    html::assert_input(&de_doc, "meta__sku", "text", Some("WDG-001"));
    html::assert_input(&de_doc, "meta__weight", "number", Some("250.0"));
}

// ── Mixed field type nesting ─────────────────────────────────────────────

/// Group containing an Array field.
///
/// Tests that the top-level group uses __ naming for the array field name,
/// while array children use bracket notation: `profile__skills[0][name]`.
fn make_group_array_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("candidates");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Candidate".to_string())),
        plural: Some(LocalizedString::Plain("Candidates".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("profile", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("bio", FieldType::Text).build(),
                FieldDefinition::builder("skills", FieldType::Array)
                    .fields(vec![
                        FieldDefinition::builder("skill", FieldType::Text)
                            .required(true)
                            .build(),
                        FieldDefinition::builder("level", FieldType::Number).build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
}

/// Array containing Collapsible wrapping a Group with mixed leaf types.
///
/// Collapsible is transparent inside array rows. Group uses bracket naming.
/// Tests: `items[0][info][0][theme]`, checkbox, number within Group>Collapsible inside Array.
fn make_array_collapsible_group_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("configs");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Config".to_string())),
        plural: Some(LocalizedString::Plain("Configs".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("wrapper", FieldType::Collapsible)
                    .fields(vec![
                        FieldDefinition::builder("label", FieldType::Text)
                            .required(true)
                            .build(),
                        FieldDefinition::builder("settings", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("theme", FieldType::Text).build(),
                                FieldDefinition::builder("dark_mode", FieldType::Checkbox).build(),
                                FieldDefinition::builder("font_size", FieldType::Number).build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
}

/// Array containing Tabs with Group inside a tab.
///
/// Tabs is transparent in Array rows. Group inside the tab uses `[0]` bracket
/// notation: `sections[0][meta][0][author]`.
fn make_array_tabs_group_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("sections", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![
                        FieldTab {
                            label: "Content".to_string(),
                            fields: vec![
                                FieldDefinition::builder("heading", FieldType::Text)
                                    .required(true)
                                    .build(),
                            ],
                            description: None,
                        },
                        FieldTab {
                            label: "Meta".to_string(),
                            fields: vec![
                                FieldDefinition::builder("meta", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("author", FieldType::Text).build(),
                                        FieldDefinition::builder("tags", FieldType::Text).build(),
                                    ])
                                    .build(),
                            ],
                            description: None,
                        },
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
}

/// Blocks where one block type contains a nested Group with mixed field types.
///
/// The hero block has `style` Group → `color` (text), `size` (number).
fn make_blocks_nested_group_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("landing");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Landing".to_string())),
        plural: Some(LocalizedString::Plain("Landings".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition {
                    block_type: "hero".to_string(),
                    fields: vec![
                        FieldDefinition::builder("headline", FieldType::Text)
                            .required(true)
                            .build(),
                        FieldDefinition::builder("style", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("color", FieldType::Text).build(),
                                FieldDefinition::builder("size", FieldType::Number).build(),
                            ])
                            .build(),
                    ],
                    label: Some(LocalizedString::Plain("Hero".to_string())),
                    ..Default::default()
                },
                BlockDefinition {
                    block_type: "paragraph".to_string(),
                    fields: vec![FieldDefinition::builder("body", FieldType::Textarea).build()],
                    label: Some(LocalizedString::Plain("Paragraph".to_string())),
                    ..Default::default()
                },
            ])
            .build(),
    ];
    def
}

/// Array containing Row wrapping a Group — tests Row transparency inside Array
/// combined with nested Group bracket naming.
///
/// Row is transparent: `entries[0][info][0][city]`, not `entries[0][layout][info][0][city]`.
fn make_array_row_group_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("contacts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Contact".to_string())),
        plural: Some(LocalizedString::Plain("Contacts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("entries", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("layout", FieldType::Row)
                    .fields(vec![
                        FieldDefinition::builder("label", FieldType::Text)
                            .required(true)
                            .build(),
                        FieldDefinition::builder("info", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("city", FieldType::Text).build(),
                                FieldDefinition::builder("zip", FieldType::Text).build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
}

// 16. Group > Array: create form renders __ for group + brackets for array
#[tokio::test]
async fn group_array_create_form_renders() {
    let app = setup_app(vec![make_group_array_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ga@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ga@test.com");

    let body = get_create_form(&app, "candidates", &cookie).await;
    let doc = html::parse(&body);

    // Group field for bio uses __ naming
    html::assert_exists(
        &doc,
        "input[name=\"profile__bio\"]",
        "group text field should use __ naming",
    );

    // Array inside group: template should have bracket naming with __ prefix
    html::assert_exists(&doc, "template", "array template");
    assert!(
        body.contains("profile__skills[__INDEX__][skill]"),
        "array child inside group should use profile__skills[idx][skill]"
    );
    assert!(
        body.contains("profile__skills[__INDEX__][level]"),
        "array child inside group should use profile__skills[idx][level]"
    );
}

// 17. Array > Collapsible > Group: template renders with collapsible transparency + group
#[tokio::test]
async fn array_collapsible_group_create_form_renders() {
    let app = setup_app(
        vec![make_array_collapsible_group_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "acg@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "acg@test.com");

    let body = get_create_form(&app, "configs", &cookie).await;
    let doc = html::parse(&body);

    html::assert_exists(&doc, "template", "array template");

    // Collapsible is transparent: label is directly under items[__INDEX__]
    assert!(
        body.contains("items[__INDEX__][label]"),
        "text field inside collapsible should use items[idx][label]"
    );
    // Group inside collapsible: uses bracket naming
    assert!(
        body.contains("items[__INDEX__][settings][theme]"),
        "group child inside collapsible should use items[idx][settings][theme]"
    );
    assert!(
        body.contains("items[__INDEX__][settings][font_size]"),
        "number field in group should use items[idx][settings][font_size]"
    );
}

// 18. Array > Collapsible > Group: validation error on required field
#[tokio::test]
async fn array_collapsible_group_validation_error() {
    let app = setup_app(
        vec![make_array_collapsible_group_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "acgval@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "acgval@test.com");

    let body = post_create(
        &app,
        "configs",
        &cookie,
        "name=Test&items[0][label]=&items[0][settings][0][theme]=dark&items[0][settings][0][dark_mode]=1&items[0][settings][0][font_size]=14",
    )
    .await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        ".form__array-row .form__error",
        "required label in collapsible should show validation error",
    );
}

// 20. Array > Tabs > Group: create form renders with tabs transparency
#[tokio::test]
async fn array_tabs_group_create_form_renders() {
    let app = setup_app(vec![make_array_tabs_group_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "atg@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "atg@test.com");

    let body = get_create_form(&app, "articles", &cookie).await;
    let doc = html::parse(&body);

    html::assert_exists(&doc, "template", "array template");

    // Tabs is transparent: heading is directly under sections[__INDEX__]
    assert!(
        body.contains("sections[__INDEX__][heading]"),
        "heading inside tab should use sections[idx][heading]"
    );
    // Group inside tab: uses bracket naming
    assert!(
        body.contains("sections[__INDEX__][meta][author]"),
        "group field inside tab should use sections[idx][meta][author]"
    );
    assert!(
        body.contains("sections[__INDEX__][meta][tags]"),
        "group field inside tab should use sections[idx][meta][tags]"
    );
}

// 21. Array > Tabs > Group: validation error on required field inside tab
#[tokio::test]
async fn array_tabs_group_validation_error() {
    let app = setup_app(vec![make_array_tabs_group_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "atgval@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "atgval@test.com");

    let body = post_create(
        &app,
        "articles",
        &cookie,
        "title=Test&sections[0][heading]=&sections[0][meta][0][author]=Bob&sections[0][meta][0][tags]=rust",
    )
    .await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        ".form__array-row .form__error",
        "required field inside array>tabs should show validation error",
    );
}

// 22. Blocks > Group: create form renders block definitions with nested group
#[tokio::test]
async fn blocks_group_create_form_renders() {
    let app = setup_app(
        vec![make_blocks_nested_group_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "bg@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "bg@test.com");

    let body = get_create_form(&app, "landing", &cookie).await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        "[data-field-type=\"blocks\"]",
        "blocks field type marker",
    );
    // Both block types present
    assert!(
        body.contains("hero") && body.contains("paragraph"),
        "both block types should be present"
    );
    // Hero block template: group uses bracket naming
    assert!(
        body.contains("content[__INDEX__][headline]"),
        "hero block should have headline field"
    );
    assert!(
        body.contains("content[__INDEX__][style][color]"),
        "hero block should have group field style[color]"
    );
    assert!(
        body.contains("content[__INDEX__][style][size]"),
        "hero block should have group field style[size]"
    );
}

// 23. Blocks > Group: validation error on required field in block with group
#[tokio::test]
async fn blocks_group_validation_error() {
    let app = setup_app(
        vec![make_blocks_nested_group_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "bgval@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "bgval@test.com");

    let body = post_create(
        &app,
        "landing",
        &cookie,
        "name=LP1&content[0][_block_type]=hero&content[0][headline]=&content[0][style][0][color]=red&content[0][style][0][size]=24",
    )
    .await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        ".form__array-row .form__error",
        "required headline in hero block should show validation error",
    );
}

// 24. Array > Row > Group: create form renders row transparency + group bracket naming
#[tokio::test]
async fn array_row_group_create_form_renders() {
    let app = setup_app(vec![make_array_row_group_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "arg@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "arg@test.com");

    let body = get_create_form(&app, "contacts", &cookie).await;
    let doc = html::parse(&body);

    // Array template should exist
    html::assert_exists(&doc, "template", "array template");
    // Row is transparent: label is directly under entries[__INDEX__]
    assert!(
        body.contains("entries[__INDEX__][label]"),
        "label inside array>row should use entries[__INDEX__][label]"
    );
    // Group inside row uses bracket naming with group index
    assert!(
        body.contains("entries[__INDEX__][info]"),
        "group inside array>row should reference entries[__INDEX__][info]"
    );
}

// 25. Array > Row > Group: validation error on required field
#[tokio::test]
async fn array_row_group_validation_error() {
    let app = setup_app(vec![make_array_row_group_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "argval@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "argval@test.com");

    // POST with empty required label
    let body = post_create(
        &app,
        "contacts",
        &cookie,
        "name=Test&entries[0][label]=&entries[0][info][0][city]=NY&entries[0][info][0][zip]=10001",
    )
    .await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        ".form__array-row .form__error",
        "required label in array>row>group should show validation error",
    );
}

// ── Group > Layout wrappers ─────────────────────────────────────────────

/// Group containing all layout wrapper types (Collapsible, Tabs, Row).
///
/// Layout wrappers are transparent — they don't add their name to the column path.
/// DB columns: name, config__theme, config__font_size, config__color,
///             config__nested__level, config__width, config__height
fn make_group_with_layouts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("widgets");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Widget".to_string())),
        plural: Some(LocalizedString::Plain("Widgets".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("config", FieldType::Group)
            .fields(vec![
                // Collapsible wrapper → transparent
                FieldDefinition::builder("wrapper", FieldType::Collapsible)
                    .fields(vec![
                        FieldDefinition::builder("theme", FieldType::Text)
                            .required(true)
                            .build(),
                        FieldDefinition::builder("font_size", FieldType::Number).build(),
                    ])
                    .build(),
                // Tabs wrapper → transparent
                FieldDefinition::builder("sections", FieldType::Tabs)
                    .tabs(vec![
                        FieldTab {
                            label: "General".to_string(),
                            fields: vec![
                                FieldDefinition::builder("color", FieldType::Text).build(),
                            ],
                            description: None,
                        },
                        FieldTab {
                            label: "Advanced".to_string(),
                            fields: vec![
                                FieldDefinition::builder("nested", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("level", FieldType::Number)
                                            .build(),
                                    ])
                                    .build(),
                            ],
                            description: None,
                        },
                    ])
                    .build(),
                // Row wrapper → transparent
                FieldDefinition::builder("layout", FieldType::Row)
                    .fields(vec![
                        FieldDefinition::builder("width", FieldType::Number).build(),
                        FieldDefinition::builder("height", FieldType::Number).build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("name".to_string()),
        ..AdminConfig::default()
    };
    def
}

// 26. Group > Layout wrappers: create form renders with transparent naming
#[tokio::test]
async fn group_layout_wrappers_create_form_renders() {
    let app = setup_app(
        vec![make_group_with_layouts_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "glw@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "glw@test.com");

    let body = get_create_form(&app, "widgets", &cookie).await;

    // Collapsible is transparent: fields use config__theme, not config__wrapper__theme
    assert!(
        body.contains(r#"name="config__theme""#),
        "collapsible child should use config__theme (transparent)"
    );
    assert!(
        body.contains(r#"name="config__font_size""#),
        "collapsible child should use config__font_size (transparent)"
    );

    // Tabs is transparent: fields use config__color, not config__sections__color
    assert!(
        body.contains(r#"name="config__color""#),
        "tab child should use config__color (transparent)"
    );

    // Group inside tab: config__nested__level
    assert!(
        body.contains(r#"name="config__nested__level""#),
        "group inside tab should use config__nested__level"
    );

    // Row is transparent: fields use config__width, not config__layout__width
    assert!(
        body.contains(r#"name="config__width""#),
        "row child should use config__width (transparent)"
    );
    assert!(
        body.contains(r#"name="config__height""#),
        "row child should use config__height (transparent)"
    );
}

// 27. Group > Layout wrappers: CRUD roundtrip — insert via query::create, verify edit form
#[tokio::test]
async fn group_layout_wrappers_crud_roundtrip() {
    let def = make_group_with_layouts_def();
    let app = setup_app(vec![def.clone(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "glwrt@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "glwrt@test.com");

    // Insert a row with flat column names
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("name".to_string(), "TestWidget".to_string()),
        ("config__theme".to_string(), "dark".to_string()),
        ("config__font_size".to_string(), "16".to_string()),
        ("config__color".to_string(), "blue".to_string()),
        ("config__nested__level".to_string(), "3".to_string()),
        ("config__width".to_string(), "800".to_string()),
        ("config__height".to_string(), "600".to_string()),
    ]);
    let doc_record = crap_cms::db::query::create(&tx, "widgets", &def, &data, None).unwrap();
    tx.commit().unwrap();

    // GET edit form and verify all values are populated
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/widgets/{}", doc_record.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;

    // Verify all field values are populated in the edit form
    let doc = html::parse(&body);

    html::assert_input(&doc, "config__theme", "text", Some("dark"));
    html::assert_input(&doc, "config__font_size", "number", Some("16.0"));
    html::assert_input(&doc, "config__color", "text", Some("blue"));
    html::assert_input(&doc, "config__nested__level", "number", Some("3.0"));
    html::assert_input(&doc, "config__width", "number", Some("800.0"));
    html::assert_input(&doc, "config__height", "number", Some("600.0"));
}

// 28. Group > Layout wrappers: validation error on required field inside collapsible
#[tokio::test]
async fn group_layout_wrappers_validation() {
    let app = setup_app(
        vec![make_group_with_layouts_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "glwval@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "glwval@test.com");

    // POST with empty required config__theme
    let body = post_create(
        &app,
        "widgets",
        &cookie,
        "name=Test&config__theme=&config__font_size=14&config__color=red&config__nested__level=1&config__width=100&config__height=50",
    )
    .await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        ".form__error",
        "required config__theme should show validation error",
    );
}

// ── Group > Array CRUD ──────────────────────────────────────────────────

// 29. Group > Array: full CRUD roundtrip via form POST
#[tokio::test]
async fn group_array_crud_roundtrip() {
    let app = setup_app(vec![make_group_array_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ga_crud@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ga_crud@test.com");

    // POST create form with Group > Array data
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/candidates")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "name=Alice&profile__bio=Dev&profile__skills[0][skill]=Rust&profile__skills[0][level]=9&profile__skills[1][skill]=Lua&profile__skills[1][level]=7",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Should redirect on success (HX-Redirect or 303)
    let status = resp.status();
    assert!(
        status.is_redirection() || resp.headers().contains_key("hx-redirect"),
        "expected redirect after successful create, got {status}"
    );

    // Verify via DB — use find to get ID, then find_by_id for full hydration
    let conn = app.pool.get().unwrap();
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("candidates").unwrap().clone()
    };
    let docs = crap_cms::db::query::find(
        &conn,
        "candidates",
        &def,
        &crap_cms::db::FindQuery::default(),
        None,
    )
    .unwrap();
    assert_eq!(docs.len(), 1);
    let doc_id = &*docs[0].id;

    let doc = crap_cms::db::query::find_by_id(&conn, "candidates", &def, doc_id, None)
        .unwrap()
        .expect("document should exist");

    // Group scalar field should be saved (reconstructed into nested object)
    let profile = doc
        .fields
        .get("profile")
        .expect("profile group should exist");
    assert_eq!(profile.get("bio").and_then(|v| v.as_str()), Some("Dev"));

    // Array should be hydrated from join table into the group object
    let skills = profile
        .get("skills")
        .expect("skills array should exist in profile");
    let arr = skills.as_array().expect("skills should be a JSON array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["skill"], "Rust");
    assert_eq!(arr[1]["skill"], "Lua");
}

// 30. Group > Collapsible > Array: full CRUD roundtrip via form POST
#[tokio::test]
async fn group_collapsible_array_crud_roundtrip() {
    // Build definition: Group "settings" > Collapsible "extras" > Array "tags"
    let mut def = CollectionDefinition::new("products");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Product".to_string())),
        plural: Some(LocalizedString::Plain("Products".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("settings", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("color", FieldType::Text).build(),
                FieldDefinition::builder("extras", FieldType::Collapsible)
                    .fields(vec![
                        FieldDefinition::builder("tags", FieldType::Array)
                            .fields(vec![
                                FieldDefinition::builder("tag", FieldType::Text)
                                    .required(true)
                                    .build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];

    let app = setup_app(vec![def, make_users_def()], vec![]);
    let user_id = create_test_user(&app, "gca@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gca@test.com");

    // Verify create form renders Array template with correct __ naming
    let body = get_create_form(&app, "products", &cookie).await;
    assert!(
        body.contains("settings__tags[__INDEX__][tag]"),
        "Group > Collapsible > Array should use settings__tags[idx][tag]"
    );

    // POST create with Array data
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/products")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "name=Widget&settings__color=blue&settings__tags[0][tag]=sale&settings__tags[1][tag]=new",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status.is_redirection() || resp.headers().contains_key("hx-redirect"),
        "expected redirect after successful create, got {status}"
    );

    // Verify via DB — use find_by_id for full hydration
    let conn = app.pool.get().unwrap();
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("products").unwrap().clone()
    };
    let docs = crap_cms::db::query::find(
        &conn,
        "products",
        &def,
        &crap_cms::db::FindQuery::default(),
        None,
    )
    .unwrap();
    assert_eq!(docs.len(), 1);
    let doc_id = &*docs[0].id;

    let doc = crap_cms::db::query::find_by_id(&conn, "products", &def, doc_id, None)
        .unwrap()
        .expect("document should exist");

    // Group scalar field reconstructed into nested object
    let settings = doc
        .fields
        .get("settings")
        .expect("settings group should exist");
    assert_eq!(settings.get("color").and_then(|v| v.as_str()), Some("blue"));

    // Array inside Group > Collapsible should be hydrated
    let tags = settings
        .get("tags")
        .expect("tags array should exist in settings");
    let arr = tags.as_array().expect("tags should be a JSON array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["tag"], "sale");
    assert_eq!(arr[1]["tag"], "new");
}

// ── Group > Array + Blocks mixed ────────────────────────────────────────

/// Collection with Group containing both Array and Blocks fields.
fn make_group_array_blocks_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("projects");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Project".to_string())),
        plural: Some(LocalizedString::Plain("Projects".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("content", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("summary", FieldType::Text).build(),
                FieldDefinition::builder("items", FieldType::Array)
                    .fields(vec![
                        FieldDefinition::builder("name", FieldType::Text)
                            .required(true)
                            .build(),
                        FieldDefinition::builder("value", FieldType::Number).build(),
                    ])
                    .build(),
                FieldDefinition::builder("sections", FieldType::Blocks)
                    .blocks(vec![
                        BlockDefinition {
                            block_type: "hero".to_string(),
                            fields: vec![
                                FieldDefinition::builder("headline", FieldType::Text).build(),
                                FieldDefinition::builder("subtitle", FieldType::Text).build(),
                            ],
                            label: Some(LocalizedString::Plain("Hero".to_string())),
                            ..Default::default()
                        },
                        BlockDefinition {
                            block_type: "text".to_string(),
                            fields: vec![
                                FieldDefinition::builder("body", FieldType::Textarea).build(),
                            ],
                            label: Some(LocalizedString::Plain("Text".to_string())),
                            ..Default::default()
                        },
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
}

// 31. Group > Array + Blocks: create form renders
#[tokio::test]
async fn group_array_blocks_create_form_renders() {
    let app = setup_app(
        vec![make_group_array_blocks_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "gab1@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gab1@test.com");

    let body = get_create_form(&app, "projects", &cookie).await;

    // Array inside group: template with __ prefix
    assert!(
        body.contains("content__items[__INDEX__][name]"),
        "Group > Array should use content__items[idx][name]"
    );
    // Blocks inside group
    assert!(
        body.contains("content__sections[__INDEX__]"),
        "Group > Blocks should use content__sections[idx]"
    );
}

// 32. Group > Array + Blocks: CRUD roundtrip
#[tokio::test]
async fn group_array_blocks_crud_roundtrip() {
    let app = setup_app(
        vec![make_group_array_blocks_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "gab2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gab2@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/projects")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "title=Proj1&content__summary=Overview\
                     &content__items[0][name]=Alpha&content__items[0][value]=10\
                     &content__items[1][name]=Beta&content__items[1][value]=20\
                     &content__sections[0][_block_type]=hero&content__sections[0][headline]=Welcome&content__sections[0][subtitle]=Hi\
                     &content__sections[1][_block_type]=text&content__sections[1][body]=Some+text",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status.is_redirection() || resp.headers().contains_key("hx-redirect"),
        "expected redirect after successful create, got {status}"
    );

    // Verify via DB
    let conn = app.pool.get().unwrap();
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("projects").unwrap().clone()
    };
    let docs = crap_cms::db::query::find(
        &conn,
        "projects",
        &def,
        &crap_cms::db::FindQuery::default(),
        None,
    )
    .unwrap();
    assert_eq!(docs.len(), 1);

    let doc = crap_cms::db::query::find_by_id(&conn, "projects", &def, &docs[0].id, None)
        .unwrap()
        .expect("document should exist");

    let content = doc
        .fields
        .get("content")
        .expect("content group should exist");
    assert_eq!(
        content.get("summary").and_then(|v| v.as_str()),
        Some("Overview")
    );

    let items = content
        .get("items")
        .expect("items array should exist in content");
    let items_arr = items.as_array().expect("items should be array");
    assert_eq!(items_arr.len(), 2);
    assert_eq!(items_arr[0]["name"], "Alpha");
    assert_eq!(items_arr[1]["name"], "Beta");

    let sections = content
        .get("sections")
        .expect("sections blocks should exist in content");
    let sections_arr = sections.as_array().expect("sections should be array");
    assert_eq!(sections_arr.len(), 2);
    assert_eq!(sections_arr[0]["_block_type"], "hero");
    assert_eq!(sections_arr[0]["headline"], "Welcome");
    assert_eq!(sections_arr[1]["_block_type"], "text");
    assert_eq!(sections_arr[1]["body"], "Some text");
}

// ── Group > Layout wrappers > Array + Blocks ────────────────────────────

/// Group with Collapsible and Tabs wrapping Array and Blocks.
fn make_group_mixed_layout_array_blocks_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("dashboards");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Dashboard".to_string())),
        plural: Some(LocalizedString::Plain("Dashboards".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("config", FieldType::Group)
            .fields(vec![
                // Collapsible > Array
                FieldDefinition::builder("extras", FieldType::Collapsible)
                    .fields(vec![
                        FieldDefinition::builder("widgets", FieldType::Array)
                            .fields(vec![
                                FieldDefinition::builder("widget_name", FieldType::Text)
                                    .required(true)
                                    .build(),
                                FieldDefinition::builder("enabled", FieldType::Checkbox).build(),
                            ])
                            .build(),
                    ])
                    .build(),
                // Tabs > Blocks
                FieldDefinition::builder("panels", FieldType::Tabs)
                    .tabs(vec![FieldTab {
                        label: "Main".to_string(),
                        fields: vec![
                            FieldDefinition::builder("blocks", FieldType::Blocks)
                                .blocks(vec![BlockDefinition {
                                    block_type: "chart".to_string(),
                                    fields: vec![
                                        FieldDefinition::builder("chart_type", FieldType::Text)
                                            .build(),
                                    ],
                                    label: Some(LocalizedString::Plain("Chart".to_string())),
                                    ..Default::default()
                                }])
                                .build(),
                        ],
                        description: None,
                    }])
                    .build(),
            ])
            .build(),
    ];
    def
}

// 33. Group > Layout > Array + Blocks: form renders
#[tokio::test]
async fn group_layout_array_blocks_create_form_renders() {
    let app = setup_app(
        vec![make_group_mixed_layout_array_blocks_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "glab1@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "glab1@test.com");

    let body = get_create_form(&app, "dashboards", &cookie).await;

    // Collapsible is transparent: config__widgets (not config__extras__widgets)
    assert!(
        body.contains("config__widgets[__INDEX__][widget_name]"),
        "Collapsible-wrapped Array should use config__widgets[idx][widget_name]"
    );
    // Tabs is transparent: config__blocks (not config__panels__blocks)
    assert!(
        body.contains("config__blocks[__INDEX__]"),
        "Tabs-wrapped Blocks should use config__blocks[idx]"
    );
}

// 34. Group > Layout > Array + Blocks: CRUD roundtrip
#[tokio::test]
async fn group_layout_array_blocks_crud_roundtrip() {
    let app = setup_app(
        vec![make_group_mixed_layout_array_blocks_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "glab2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "glab2@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/dashboards")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "name=Dash1\
                     &config__widgets[0][widget_name]=Weather&config__widgets[0][enabled]=1\
                     &config__blocks[0][_block_type]=chart&config__blocks[0][chart_type]=bar",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status.is_redirection() || resp.headers().contains_key("hx-redirect"),
        "expected redirect after successful create, got {status}"
    );

    let conn = app.pool.get().unwrap();
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("dashboards").unwrap().clone()
    };
    let docs = crap_cms::db::query::find(
        &conn,
        "dashboards",
        &def,
        &crap_cms::db::FindQuery::default(),
        None,
    )
    .unwrap();
    assert_eq!(docs.len(), 1);

    let doc = crap_cms::db::query::find_by_id(&conn, "dashboards", &def, &docs[0].id, None)
        .unwrap()
        .expect("document should exist");

    let config = doc.fields.get("config").expect("config group should exist");

    let widgets = config
        .get("widgets")
        .expect("widgets array should exist in config");
    let w_arr = widgets.as_array().expect("widgets should be array");
    assert_eq!(w_arr.len(), 1);
    assert_eq!(w_arr[0]["widget_name"], "Weather");

    let blocks = config.get("blocks").expect("blocks should exist in config");
    let b_arr = blocks.as_array().expect("blocks should be array");
    assert_eq!(b_arr.len(), 1);
    assert_eq!(b_arr[0]["_block_type"], "chart");
    assert_eq!(b_arr[0]["chart_type"], "bar");
}

// ── Localized Group > Array + Blocks ────────────────────────────────────

/// Collection with localized Group containing localized Array and Blocks.
fn make_localized_group_array_blocks_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Page".to_string())),
        plural: Some(LocalizedString::Plain("Pages".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("slug", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("content", FieldType::Group)
            .localized(true)
            .fields(vec![
                FieldDefinition::builder("headline", FieldType::Text)
                    .localized(true)
                    .build(),
                FieldDefinition::builder("items", FieldType::Array)
                    .localized(true)
                    .fields(vec![
                        FieldDefinition::builder("label", FieldType::Text)
                            .required(true)
                            .build(),
                    ])
                    .build(),
                FieldDefinition::builder("sections", FieldType::Blocks)
                    .localized(true)
                    .blocks(vec![BlockDefinition {
                        block_type: "text".to_string(),
                        fields: vec![FieldDefinition::builder("body", FieldType::Textarea).build()],
                        label: Some(LocalizedString::Plain("Text".to_string())),
                        ..Default::default()
                    }])
                    .build(),
            ])
            .build(),
    ];
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

// 35. Localized Group > Array + Blocks: CRUD roundtrip across two locales
#[tokio::test]
async fn localized_group_array_blocks_crud_roundtrip() {
    let config = make_locale_config();
    let locale_config = config.locale.clone();
    let app = setup_app_with_config(
        vec![make_localized_group_array_blocks_def(), make_users_def()],
        vec![],
        config,
    );
    let user_id = create_test_user(&app, "lgab1@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "lgab1@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("pages").unwrap().clone()
    };

    // Create in EN: headline, 2 array rows, 1 block
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let en_locale_ctx = LocaleContext::from_locale_string(Some("en"), &locale_config).unwrap();
    let en_data = std::collections::HashMap::from([
        ("slug".to_string(), "hello".to_string()),
        ("content__headline".to_string(), "EN Headline".to_string()),
    ]);
    let doc_record =
        crap_cms::db::query::create(&tx, "pages", &def, &en_data, en_locale_ctx.as_ref()).unwrap();

    // Save array + block data for EN
    let en_join_data = std::collections::HashMap::from([
        (
            "content__items".to_string(),
            json!([{"label": "EN One"}, {"label": "EN Two"}]),
        ),
        (
            "content__sections".to_string(),
            json!([{"_block_type": "text", "body": "EN body"}]),
        ),
    ]);
    crap_cms::db::query::save_join_table_data(
        &tx,
        "pages",
        &def.fields,
        &doc_record.id,
        &en_join_data,
        en_locale_ctx.as_ref(),
    )
    .unwrap();

    // Update DE locale
    let de_locale_ctx = LocaleContext::from_locale_string(Some("de"), &locale_config).unwrap();
    let de_data = std::collections::HashMap::from([(
        "content__headline".to_string(),
        "DE Schlagzeile".to_string(),
    )]);
    crap_cms::db::query::update(
        &tx,
        "pages",
        &def,
        &doc_record.id,
        &de_data,
        de_locale_ctx.as_ref(),
    )
    .unwrap();

    // Save array + block data for DE
    let de_join_data = std::collections::HashMap::from([
        ("content__items".to_string(), json!([{"label": "DE Eins"}])),
        (
            "content__sections".to_string(),
            json!([{"_block_type": "text", "body": "DE Text"}]),
        ),
    ]);
    crap_cms::db::query::save_join_table_data(
        &tx,
        "pages",
        &def.fields,
        &doc_record.id,
        &de_join_data,
        de_locale_ctx.as_ref(),
    )
    .unwrap();
    tx.commit().unwrap();

    // Verify EN data via find_by_id with locale context
    let conn = app.pool.get().unwrap();
    let en_doc_record = crap_cms::db::query::find_by_id(
        &conn,
        "pages",
        &def,
        &doc_record.id,
        en_locale_ctx.as_ref(),
    )
    .unwrap()
    .expect("EN document should exist");

    let content = en_doc_record
        .fields
        .get("content")
        .expect("content group should exist");
    assert_eq!(
        content.get("headline").and_then(|v| v.as_str()),
        Some("EN Headline")
    );
    let en_items = content.get("items").expect("EN items should exist");
    let en_items_arr = en_items.as_array().expect("EN items should be array");
    assert_eq!(en_items_arr.len(), 2, "EN should have 2 array rows");
    assert_eq!(en_items_arr[0]["label"], "EN One");
    assert_eq!(en_items_arr[1]["label"], "EN Two");
    let en_sections = content.get("sections").expect("EN sections should exist");
    let en_sections_arr = en_sections.as_array().expect("EN sections should be array");
    assert_eq!(en_sections_arr.len(), 1);
    assert_eq!(en_sections_arr[0]["body"], "EN body");

    // Verify DE data via find_by_id
    let de_doc_record = crap_cms::db::query::find_by_id(
        &conn,
        "pages",
        &def,
        &doc_record.id,
        de_locale_ctx.as_ref(),
    )
    .unwrap()
    .expect("DE document should exist");

    let de_content = de_doc_record
        .fields
        .get("content")
        .expect("DE content group should exist");
    assert_eq!(
        de_content.get("headline").and_then(|v| v.as_str()),
        Some("DE Schlagzeile")
    );
    let de_items = de_content.get("items").expect("DE items should exist");
    let de_items_arr = de_items.as_array().expect("DE items should be array");
    assert_eq!(de_items_arr.len(), 1, "DE should have 1 array row");
    assert_eq!(de_items_arr[0]["label"], "DE Eins");
    let de_sections = de_content
        .get("sections")
        .expect("DE sections should exist");
    let de_sections_arr = de_sections.as_array().expect("DE sections should be array");
    assert_eq!(de_sections_arr.len(), 1);
    assert_eq!(de_sections_arr[0]["body"], "DE Text");

    // Verify edit form shows correct headline per locale
    let en_body = get_edit_form_with_locale(&app, "pages", &doc_record.id, &cookie, "en").await;
    let en_html = html::parse(&en_body);
    html::assert_input(&en_html, "content__headline", "text", Some("EN Headline"));

    let de_body = get_edit_form_with_locale(&app, "pages", &doc_record.id, &cookie, "de").await;
    let de_html = html::parse(&de_body);
    html::assert_input(
        &de_html,
        "content__headline",
        "text",
        Some("DE Schlagzeile"),
    );
}

// 36. Localized Group > Array + Blocks: non-default locale editable check
#[tokio::test]
async fn localized_group_array_blocks_non_default_editable() {
    let config = make_locale_config();
    let app = setup_app_with_config(
        vec![make_localized_group_array_blocks_def(), make_users_def()],
        vec![],
        config,
    );
    let user_id = create_test_user(&app, "lgab2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "lgab2@test.com");

    let body = get_create_form_with_locale(&app, "pages", &cookie, "de").await;
    let doc = html::parse(&body);

    // Array + Blocks inside localized Group should be editable in DE
    // (no "shared_field" badge, add button visible)
    html::assert_not_exists(
        &doc,
        "[data-field-name=\"content__items\"] .form__locale-badge",
        "localized array content__items should not show shared_field badge in DE",
    );
    html::assert_not_exists(
        &doc,
        "[data-field-name=\"content__sections\"] .form__locale-badge",
        "localized blocks content__sections should not show shared_field badge in DE",
    );
    // Add buttons should be visible for localized fields
    html::assert_exists(
        &doc,
        "[data-field-name=\"content__items\"] [data-action=\"add-array-row\"]",
        "localized array should have add button in DE",
    );
    html::assert_exists(
        &doc,
        "[data-field-name=\"content__sections\"] [data-action=\"add-block-row\"]",
        "localized blocks should have add button in DE",
    );

    // slug (non-localized, outside group) should be locked
    html::assert_exists(
        &doc,
        "input[name=\"slug\"][readonly]",
        "non-localized slug should be readonly in DE",
    );
}

// ── Mixed locale Group: localized Array + non-localized Blocks ─────────

/// Group (NOT localized) containing localized Array and non-localized Blocks.
fn make_mixed_locale_group_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("tags", FieldType::Array)
                    .localized(true)
                    .fields(vec![
                        FieldDefinition::builder("tag", FieldType::Text)
                            .required(true)
                            .build(),
                    ])
                    .build(),
                FieldDefinition::builder("layout", FieldType::Blocks)
                    .blocks(vec![BlockDefinition {
                        block_type: "widget".to_string(),
                        fields: vec![FieldDefinition::builder("kind", FieldType::Text).build()],
                        label: Some(LocalizedString::Plain("Widget".to_string())),
                        ..Default::default()
                    }])
                    .build(),
            ])
            .build(),
    ];
    def
}

// 37. Mixed locale Group: localized Array editable, non-localized Blocks locked
#[tokio::test]
async fn mixed_locale_group_array_editable_blocks_locked() {
    let config = make_locale_config();
    let app = setup_app_with_config(
        vec![make_mixed_locale_group_def(), make_users_def()],
        vec![],
        config,
    );
    let user_id = create_test_user(&app, "mlg1@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "mlg1@test.com");

    let body = get_create_form_with_locale(&app, "articles", &cookie, "de").await;
    let doc = html::parse(&body);

    // Localized array: meta__tags should be editable (no badge, add button present)
    html::assert_not_exists(
        &doc,
        "[data-field-name=\"meta__tags\"] .form__locale-badge",
        "localized meta__tags should not show shared_field badge",
    );
    html::assert_exists(
        &doc,
        "[data-field-name=\"meta__tags\"] [data-action=\"add-array-row\"]",
        "localized meta__tags should have add button in DE",
    );

    // Non-localized blocks: meta__layout should be locked (badge shown, no add button)
    html::assert_exists(
        &doc,
        "[data-field-name=\"meta__layout\"] .form__locale-badge",
        "non-localized meta__layout should show shared_field badge in DE",
    );
    html::assert_not_exists(
        &doc,
        "[data-field-name=\"meta__layout\"] [data-action=\"add-block-row\"]",
        "non-localized meta__layout should NOT have add button in DE",
    );

    // title (non-localized, outside group) should be readonly in DE
    html::assert_exists(
        &doc,
        "input[name=\"title\"][readonly]",
        "non-localized title should be readonly in DE",
    );
}

// 38. Mixed locale Group: CRUD roundtrip across two locales
#[tokio::test]
async fn mixed_locale_group_crud_roundtrip() {
    let config = make_locale_config();
    let locale_config = config.locale.clone();
    let app = setup_app_with_config(
        vec![make_mixed_locale_group_def(), make_users_def()],
        vec![],
        config,
    );
    let user_id = create_test_user(&app, "mlg2@test.com", "pass123");
    let _cookie = make_auth_cookie(&app, &user_id, "mlg2@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };

    // Create in EN
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let en_locale_ctx = LocaleContext::from_locale_string(Some("en"), &locale_config).unwrap();
    let en_data =
        std::collections::HashMap::from([("title".to_string(), "Test Article".to_string())]);
    let doc_record =
        crap_cms::db::query::create(&tx, "articles", &def, &en_data, en_locale_ctx.as_ref())
            .unwrap();

    // Save EN tags (localized) + layout blocks (non-localized)
    let en_join_data = std::collections::HashMap::from([
        (
            "meta__tags".to_string(),
            json!([{"tag": "rust"}, {"tag": "wasm"}]),
        ),
        (
            "meta__layout".to_string(),
            json!([{"_block_type": "widget", "kind": "sidebar"}]),
        ),
    ]);
    crap_cms::db::query::save_join_table_data(
        &tx,
        "articles",
        &def.fields,
        &doc_record.id,
        &en_join_data,
        en_locale_ctx.as_ref(),
    )
    .unwrap();

    // Save DE tags only (layout is non-localized, shared)
    let de_locale_ctx = LocaleContext::from_locale_string(Some("de"), &locale_config).unwrap();
    let de_join_data =
        std::collections::HashMap::from([("meta__tags".to_string(), json!([{"tag": "rost"}]))]);
    crap_cms::db::query::save_join_table_data(
        &tx,
        "articles",
        &def.fields,
        &doc_record.id,
        &de_join_data,
        de_locale_ctx.as_ref(),
    )
    .unwrap();
    tx.commit().unwrap();

    // Verify EN data via find_by_id
    let conn = app.pool.get().unwrap();
    let en_doc = crap_cms::db::query::find_by_id(
        &conn,
        "articles",
        &def,
        &doc_record.id,
        en_locale_ctx.as_ref(),
    )
    .unwrap()
    .expect("EN document should exist");

    let meta = en_doc.fields.get("meta").expect("meta group should exist");
    let en_tags = meta.get("tags").expect("EN tags should exist");
    let en_tags_arr = en_tags.as_array().expect("EN tags should be array");
    assert_eq!(en_tags_arr.len(), 2, "EN should have 2 tags");
    assert_eq!(en_tags_arr[0]["tag"], "rust");
    assert_eq!(en_tags_arr[1]["tag"], "wasm");

    let layout = meta.get("layout").expect("layout blocks should exist");
    let layout_arr = layout.as_array().expect("layout should be array");
    assert_eq!(layout_arr.len(), 1);
    assert_eq!(layout_arr[0]["kind"], "sidebar");

    // Verify DE data: different tags, same layout
    let de_doc = crap_cms::db::query::find_by_id(
        &conn,
        "articles",
        &def,
        &doc_record.id,
        de_locale_ctx.as_ref(),
    )
    .unwrap()
    .expect("DE document should exist");

    let de_meta = de_doc
        .fields
        .get("meta")
        .expect("DE meta group should exist");
    let de_tags = de_meta.get("tags").expect("DE tags should exist");
    let de_tags_arr = de_tags.as_array().expect("DE tags should be array");
    assert_eq!(de_tags_arr.len(), 1, "DE should have 1 tag");
    assert_eq!(de_tags_arr[0]["tag"], "rost");

    // Non-localized layout should be the same in DE
    let de_layout = de_meta
        .get("layout")
        .expect("DE layout blocks should exist");
    let de_layout_arr = de_layout.as_array().expect("DE layout should be array");
    assert_eq!(de_layout_arr.len(), 1, "DE should share the same layout");
    assert_eq!(de_layout_arr[0]["kind"], "sidebar");
}

// ── Deep Blocks nesting (Blocks-in-Blocks) ─────────────────────────────

/// Blocks containing nested Blocks, Arrays, etc. All stored as JSON in single join table.
fn make_deep_blocks_nesting_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("layouts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Layout".to_string())),
        plural: Some(LocalizedString::Plain("Layouts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition {
                    block_type: "container".to_string(),
                    fields: vec![
                        FieldDefinition::builder("label", FieldType::Text).build(),
                        FieldDefinition::builder("inner", FieldType::Blocks)
                            .blocks(vec![BlockDefinition {
                                block_type: "card".to_string(),
                                fields: vec![
                                    FieldDefinition::builder("title", FieldType::Text)
                                        .required(true)
                                        .build(),
                                    FieldDefinition::builder("items", FieldType::Array)
                                        .fields(vec![
                                            FieldDefinition::builder("name", FieldType::Text)
                                                .required(true)
                                                .build(),
                                            FieldDefinition::builder("qty", FieldType::Number)
                                                .build(),
                                        ])
                                        .build(),
                                    FieldDefinition::builder("footer", FieldType::Blocks)
                                        .blocks(vec![BlockDefinition {
                                            block_type: "link".to_string(),
                                            fields: vec![
                                                FieldDefinition::builder("url", FieldType::Text)
                                                    .build(),
                                                FieldDefinition::builder("text", FieldType::Text)
                                                    .build(),
                                            ],
                                            label: Some(LocalizedString::Plain("Link".to_string())),
                                            ..Default::default()
                                        }])
                                        .build(),
                                ],
                                label: Some(LocalizedString::Plain("Card".to_string())),
                                ..Default::default()
                            }])
                            .build(),
                    ],
                    label: Some(LocalizedString::Plain("Container".to_string())),
                    ..Default::default()
                },
                BlockDefinition {
                    block_type: "simple".to_string(),
                    fields: vec![FieldDefinition::builder("body", FieldType::Textarea).build()],
                    label: Some(LocalizedString::Plain("Simple".to_string())),
                    ..Default::default()
                },
            ])
            .build(),
    ];
    def
}

// 39. Deep Blocks nesting: form renders nested block type templates
#[tokio::test]
async fn deep_blocks_nesting_create_form_renders() {
    let app = setup_app(
        vec![make_deep_blocks_nesting_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "dbn1@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "dbn1@test.com");

    let body = get_create_form(&app, "layouts", &cookie).await;
    let doc = html::parse(&body);

    html::assert_exists(
        &doc,
        "[data-field-type=\"blocks\"]",
        "blocks field type marker",
    );
    // Top-level block types present
    assert!(
        body.contains("container") && body.contains("simple"),
        "both top-level block types should be present"
    );
    // Nested block type templates
    assert!(
        body.contains("content[__INDEX__]"),
        "top-level block template should exist"
    );
}

// 40. Deep Blocks nesting: CRUD roundtrip with deeply nested data
#[tokio::test]
async fn deep_blocks_nesting_crud_roundtrip() {
    let app = setup_app(
        vec![make_deep_blocks_nesting_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "dbn2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "dbn2@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/layouts")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "name=Layout1\
                     &content[0][_block_type]=container&content[0][label]=Section1\
                     &content[0][inner][0][_block_type]=card&content[0][inner][0][title]=Card1\
                     &content[0][inner][0][items][0][name]=Item1&content[0][inner][0][items][0][qty]=5\
                     &content[0][inner][0][footer][0][_block_type]=link\
                     &content[0][inner][0][footer][0][url]=/home&content[0][inner][0][footer][0][text]=Home\
                     &content[1][_block_type]=simple&content[1][body]=Just+text",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status.is_redirection() || resp.headers().contains_key("hx-redirect"),
        "expected redirect after successful create, got {status}"
    );

    // Verify via DB
    let conn = app.pool.get().unwrap();
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("layouts").unwrap().clone()
    };
    let docs = crap_cms::db::query::find(
        &conn,
        "layouts",
        &def,
        &crap_cms::db::FindQuery::default(),
        None,
    )
    .unwrap();
    assert_eq!(docs.len(), 1);

    let doc = crap_cms::db::query::find_by_id(&conn, "layouts", &def, &docs[0].id, None)
        .unwrap()
        .expect("document should exist");

    let content = doc
        .fields
        .get("content")
        .expect("content blocks should exist");
    let content_arr = content.as_array().expect("content should be array");
    assert_eq!(content_arr.len(), 2);

    // Block 0: container
    assert_eq!(content_arr[0]["_block_type"], "container");
    assert_eq!(content_arr[0]["label"], "Section1");

    // Block 0 → inner[0]: card
    let inner = content_arr[0]
        .get("inner")
        .expect("container should have inner blocks");
    let inner_arr = inner.as_array().expect("inner should be array");
    assert_eq!(inner_arr.len(), 1);
    assert_eq!(inner_arr[0]["_block_type"], "card");
    assert_eq!(inner_arr[0]["title"], "Card1");

    // Block 0 → inner[0] → items
    let items = inner_arr[0]
        .get("items")
        .expect("card should have items array");
    let items_arr = items.as_array().expect("items should be array");
    assert_eq!(items_arr.len(), 1);
    assert_eq!(items_arr[0]["name"], "Item1");
    assert_eq!(items_arr[0]["qty"], "5");

    // Block 0 → inner[0] → footer
    let footer = inner_arr[0]
        .get("footer")
        .expect("card should have footer blocks");
    let footer_arr = footer.as_array().expect("footer should be array");
    assert_eq!(footer_arr.len(), 1);
    assert_eq!(footer_arr[0]["_block_type"], "link");
    assert_eq!(footer_arr[0]["url"], "/home");

    // Block 1: simple
    assert_eq!(content_arr[1]["_block_type"], "simple");
    assert_eq!(content_arr[1]["body"], "Just text");
}

// ── Group > Group > Array e2e ────────────────────────────────────────────

/// Collection with double-nested groups containing an Array.
fn make_double_nested_group_array_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("reports");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Report".to_string())),
        plural: Some(LocalizedString::Plain("Reports".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("outer", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("summary", FieldType::Text).build(),
                FieldDefinition::builder("inner", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("label", FieldType::Text).build(),
                        FieldDefinition::builder("items", FieldType::Array)
                            .fields(vec![
                                FieldDefinition::builder("name", FieldType::Text)
                                    .required(true)
                                    .build(),
                                FieldDefinition::builder("qty", FieldType::Number).build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    def
}

// 41. Group > Group > Array: form renders with correct prefixed names
#[tokio::test]
async fn double_nested_group_array_create_form_renders() {
    let app = setup_app(
        vec![make_double_nested_group_array_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "dng1@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "dng1@test.com");

    let body = get_create_form(&app, "reports", &cookie).await;
    let doc = html::parse(&body);

    // Verify double-prefixed array template names
    html::assert_exists(
        &doc,
        "[data-field-name=\"outer__inner__items\"]",
        "double-nested array field should exist",
    );
    html::assert_exists(
        &doc,
        "input[name=\"outer__inner__items[__INDEX__][name]\"]",
        "template should have double-prefixed name for sub-fields",
    );
}

// 42. Group > Group > Array: CRUD roundtrip
#[tokio::test]
async fn double_nested_group_array_crud_roundtrip() {
    let app = setup_app(
        vec![make_double_nested_group_array_def(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "dng2@test.com", "pass123");
    let _cookie = make_auth_cookie(&app, &user_id, "dng2@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("reports").unwrap().clone()
    };

    // Create via query API
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("title".to_string(), "Test Report".to_string()),
        ("outer__summary".to_string(), "A summary".to_string()),
        ("outer__inner__label".to_string(), "Deep Label".to_string()),
    ]);
    let doc_record = crap_cms::db::query::create(&tx, "reports", &def, &data, None).unwrap();

    // Save array rows
    let join_data = std::collections::HashMap::from([(
        "outer__inner__items".to_string(),
        json!([
            {"name": "Item1", "qty": "10"},
            {"name": "Item2", "qty": "20"},
        ]),
    )]);
    crap_cms::db::query::save_join_table_data(
        &tx,
        "reports",
        &def.fields,
        &doc_record.id,
        &join_data,
        None,
    )
    .unwrap();
    tx.commit().unwrap();

    // Verify via find_by_id
    let conn = app.pool.get().unwrap();
    let doc = crap_cms::db::query::find_by_id(&conn, "reports", &def, &doc_record.id, None)
        .unwrap()
        .expect("document should exist");

    let outer = doc.fields.get("outer").expect("outer group should exist");
    assert_eq!(
        outer.get("summary").and_then(|v| v.as_str()),
        Some("A summary")
    );

    let inner = outer.get("inner").expect("inner group should exist");
    assert_eq!(
        inner.get("label").and_then(|v| v.as_str()),
        Some("Deep Label")
    );

    let items = inner.get("items").expect("items array should exist");
    let items_arr = items.as_array().expect("items should be array");
    assert_eq!(items_arr.len(), 2);
    assert_eq!(items_arr[0]["name"], "Item1");
    assert_eq!(items_arr[0]["qty"], 10.0);
    assert_eq!(items_arr[1]["name"], "Item2");
    assert_eq!(items_arr[1]["qty"], 20.0);
}
