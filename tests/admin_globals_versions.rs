//! Global versioning integration tests for the admin HTTP handlers.
//!
//! Covers: the versions page, draft save, unpublish and version restore.

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

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use admin_globals_support::{
    TEST_CSRF, auth_and_csrf, body_string, create_test_user, make_auth_cookie, make_global_def,
    make_users_def, make_versioned_global_def, setup_app,
};
use crap_cms::db::query;

#[tokio::test]
async fn global_versions_page_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gv@test.com");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Test+Site&tagline=Hello"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Global update should succeed, got {status}"
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/globals/site_config/versions")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("version") || body.to_lowercase().contains("history"),
        "Versions page should contain version-related content"
    );
}

#[tokio::test]
async fn global_versions_page_non_versioned_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "gvr@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gvr@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/settings/versions")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::FOUND
            || status == StatusCode::TEMPORARY_REDIRECT,
        "Non-versioned global versions page should redirect, got {status}"
    );
}

#[tokio::test]
async fn global_update_with_draft_action() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gdraft@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gdraft@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "site_name=Draft+Site&tagline=WIP&_action=save_draft",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Draft save should succeed, got {status}"
    );
}

#[tokio::test]
async fn global_update_unpublish_action() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gunpub@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gunpub@test.com");

    let _resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Published+Site&tagline=Live"))
                .unwrap(),
        )
        .await
        .unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "site_name=Published+Site&tagline=Live&_action=unpublish",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Unpublish should succeed, got {status}"
    );
}

#[tokio::test]
async fn global_restore_version() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "grestore@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "grestore@test.com");

    let _resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Version+1&tagline=First"))
                .unwrap(),
        )
        .await
        .unwrap();

    let _resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Version+2&tagline=Second"))
                .unwrap(),
        )
        .await
        .unwrap();

    let conn = app.pool.get().unwrap();
    let versions = query::list_versions(
        &conn,
        "_global_site_config",
        "default",
        false,
        Some(10),
        None,
    )
    .expect("list versions");
    drop(conn);

    let v = versions
        .first()
        .expect("each published save records a version");

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post(format!(
                "/admin/globals/site_config/versions/{}/restore",
                v.id
            ))
            .header("cookie", auth_and_csrf(&cookie))
            .header("X-CSRF-Token", TEST_CSRF)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Restore should succeed, got {status}"
    );
}

#[tokio::test]
async fn global_restore_non_versioned_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "gnvr@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gnvr@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/settings/versions/fake-version-id/restore")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::FOUND
            || status == StatusCode::TEMPORARY_REDIRECT
            || status == StatusCode::OK,
        "Non-versioned restore should redirect, got {status}"
    );
}

#[tokio::test]
async fn global_versioned_edit_shows_version_info() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gver@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gver@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/site_config")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("version") || body.contains("draft"),
        "Versioned global edit should show version/draft info"
    );
}

#[tokio::test]
async fn global_unpublish() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gunpub@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gunpub@test.com");

    let _resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=My+Site"))
                .unwrap(),
        )
        .await
        .unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=My+Site&_action=unpublish"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Global unpublish should succeed, got {status}"
    );
}

#[tokio::test]
async fn global_version_history_page() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gverpage@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gverpage@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/site_config/versions")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("version"),
        "Global versions page should contain 'version'"
    );
}

#[tokio::test]
async fn global_non_versioned_versions_page_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "gnv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gnv@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/settings/versions")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::FOUND
            || status == StatusCode::TEMPORARY_REDIRECT,
        "Non-versioned global versions page should redirect, got {status}"
    );
}

#[tokio::test]
async fn global_restore_version_non_versioned_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "grest@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "grest@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/settings/versions/fake-version/restore")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Global restore on non-versioned should redirect, got {status}"
    );
}

#[tokio::test]
async fn global_save_as_draft() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "gdraft@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "gdraft@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_name=Draft+Value&_action=save_draft"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Global save as draft should succeed, got {status}"
    );
}

#[tokio::test]
async fn versioned_global_edit_returns_200() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "verglobal@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "verglobal@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/site_config")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn versioned_global_update_as_draft() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "vergdraft@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "vergdraft@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/site_config")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("site_title=Draft+Title&_action=save_draft"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Global draft save should succeed, got {status}"
    );
}

#[tokio::test]
async fn versioned_global_versions_page() {
    let app = setup_app(vec![make_users_def()], vec![make_versioned_global_def()]);
    let user_id = create_test_user(&app, "vergver@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "vergver@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/site_config/versions")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn non_versioned_global_versions_page_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "nonverglob@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nonverglob@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/settings/versions")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "Non-versioned global versions page should redirect"
    );
}

#[tokio::test]
async fn global_restore_nonversioned_redirects() {
    let app = setup_app(vec![make_users_def()], vec![make_global_def()]);
    let user_id = create_test_user(&app, "grestnv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "grestnv@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/settings/versions/fake-version-id/restore")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "Restore on non-versioned global should redirect"
    );
}
