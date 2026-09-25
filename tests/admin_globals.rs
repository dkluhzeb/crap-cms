//! Global edit/update integration tests for the admin HTTP handlers.
//!
//! Covers: the edit form, saves (redirects, hook aborts, unknown locales,
//! validate), unknown globals, and localized globals. Versioning lives in
//! `admin_globals_versions.rs`, access in `admin_globals_access.rs`, the
//! dashboard in `admin_dashboard.rs`, upload serving in
//! `admin_upload_serve.rs`, CSRF / CORS / the access gate in
//! `admin_request_security.rs`.

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

mod admin_globals_support;

use std::fs;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use admin_globals_support::{
    TEST_CSRF, auth_and_csrf, body_string, create_test_user, make_auth_cookie, make_global_def,
    make_locale_config, make_localized_global_def, make_users_def, setup_app,
    setup_app_with_config,
};
use crap_cms::config::CrapConfig;

#[tokio::test]
async fn global_edit_form_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "global@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "global@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/settings")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn global_update_action() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "global_update@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "global_update@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/settings")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=My+CMS"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Global update should redirect or HX-Redirect, got {status}"
    );
}

/// Regression: a `before_change` abort on a global save redirected back to the
/// edit form with no message, so the rejected save looked like one that went
/// through and reverted. It must toast the hook's message, as the collection
/// edit form does.
#[tokio::test]
async fn global_update_surfaces_a_hook_abort() {
    let mut def = make_global_def();
    def.hooks.before_change = vec!["hooks.guard.refuse".into()];

    let app = setup_app(vec![make_users_def()], vec![def]);

    let hooks_dir = app._tmp.path().join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(
        hooks_dir.join("guard.lua"),
        "local M = {}\nfunction M.refuse(ctx)\n    error(\"site name is frozen until launch\")\nend\nreturn M\n",
    )
    .unwrap();

    let user_id = create_test_user(&app, "global_hook_abort@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "global_hook_abort@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/settings")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Early"))
                .unwrap(),
        )
        .await
        .unwrap();

    let toast = resp
        .headers()
        .get("X-Crap-Toast")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        toast.contains("site name is frozen until launch"),
        "the hook's message must reach the editor, got status {} toast {toast:?}",
        resp.status()
    );
}

/// Regression: an unknown `_locale` on a global update used to be silently
/// swallowed (`from_locale_string(...).unwrap_or(None)`) into "no locale
/// context" — bare-column writes on a localized global. Must 422 instead.
#[tokio::test]
async fn global_update_with_unknown_locale_is_rejected() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();
    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "badlocale_global@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "badlocale_global@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/l10n_settings")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("welcome_text=Nope&_locale=xx"))
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

/// Regression twin for the globals validate endpoint.
#[tokio::test]
async fn global_validate_with_unknown_locale_is_rejected() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();
    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "badlocale_gvalidate@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "badlocale_gvalidate@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/l10n_settings/validate")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "data": { "welcome_text": "T" }, "locale": "xx" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("\"valid\":false") && body.contains("Invalid locale"),
        "global validate must reject the unknown locale, got: {body}"
    );
}

#[tokio::test]
async fn global_update_returns_redirect() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "global_redir@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "global_redir@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/settings")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Updated+Site"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Global update should redirect or HX-Redirect, got {status}"
    );
}

#[tokio::test]
async fn global_nonexistent_returns_404() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "gnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/nonexistent")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn global_update_nonexistent_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "gupdnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gupdnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/nonexistent_global")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Test"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::FOUND
            || status == StatusCode::TEMPORARY_REDIRECT,
        "Update nonexistent global should redirect, got {status}"
    );
}

#[tokio::test]
async fn localized_global_edit_returns_200() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();
    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "lglobal@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "lglobal@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/l10n_settings?locale=en")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn localized_global_edit_non_default_locale() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();
    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "lglobal2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "lglobal2@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/l10n_settings")
                .header("cookie", format!("{}; crap_editor_locale=de", &cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn localized_global_update_with_locale() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();
    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "lglobal3@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "lglobal3@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/l10n_settings")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "welcome_text=Willkommen&max_items=10&_locale=de",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Localized global update should succeed, got {status}"
    );
}

#[tokio::test]
async fn global_update_with_locale() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();

    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "globalloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "globalloc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/l10n_settings")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "site_title=Localized+Title&description=Desc&_locale=de",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Global update with locale should succeed, got {status}"
    );
}

#[tokio::test]
async fn global_edit_nonexistent_returns_404() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "globnon@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "globnon@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/nonexistent")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn global_edit_with_locale() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.locale = make_locale_config();

    let app = setup_app_with_config(
        vec![make_users_def()],
        vec![make_localized_global_def()],
        config,
    );
    let user_id = create_test_user(&app, "geditloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "geditloc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/l10n_settings")
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
        "Should show locale selector"
    );
}
