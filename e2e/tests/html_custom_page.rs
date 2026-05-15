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

//! Custom pages declared via `crap.pages.register` and rendered from
//! a user-supplied `pages/{slug}.hbs` template.

use std::fs;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms::config::CrapConfig;
use crap_cms_e2e::helpers::*;

// ── custom_page_renders_when_registered_and_template_exists ──────────────

#[tokio::test]
async fn custom_page_renders_when_registered_and_template_exists() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // Register the page via init.lua.
    fs::write(
        tmp.path().join("init.lua"),
        r#"
crap.pages.register("status", {
    section = "Tools",
    label = "Status",
    icon = "monitoring",
})
"#,
    )
    .expect("write init.lua");

    // Page template lives at `templates/pages/<slug>.hbs` in the config dir.
    let pages_dir = tmp.path().join("templates").join("pages");
    fs::create_dir_all(&pages_dir).expect("mkdir templates/pages");
    fs::write(
        pages_dir.join("status.hbs"),
        r"
{{#> layout/main}}
<section class='custom-status'>
    <h1>Status Page Content</h1>
    <p>This is a Lua-registered custom admin page.</p>
</section>
{{/layout/main}}
",
    )
    .expect("write template");

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.admin.dev_mode = true;

    let app = setup_app_at(vec![make_users_def()], vec![], config, tmp);
    let user_id = create_test_user(&app, "cp@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cp@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/p/status")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "custom page should render");
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Status Page Content"),
        "rendered body should include the template's content"
    );
}

// ── custom_page_missing_template_returns_404 ─────────────────────────────

#[tokio::test]
async fn custom_page_unknown_slug_returns_404() {
    let HtmlTestCtx { app, cookie, .. } =
        setup_html_test(vec![make_users_def()], vec![], "cp404@test.com", "pass123");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/p/unknown-page")
                .header("Cookie", auth_and_csrf(&cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "unknown custom page should 404"
    );
}
