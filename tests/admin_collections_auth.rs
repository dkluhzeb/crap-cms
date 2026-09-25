//! Auth-collection integration tests for the admin HTTP handlers.
//!
//! Covers: the password field on auth-collection create/edit forms and
//! actions, and the account-lock box on the user edit form.

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

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use crap_cms::{
    core::collection::CollectionDefinition,
    db::query,
    service::{ServiceContext, auth::lock_user},
};

use admin_collections_support::{
    TEST_CSRF, TestApp, auth_and_csrf, body_string, create_test_user, make_auth_cookie,
    make_users_def, setup_app, write_access_hooks,
};

/// A users collection whose `access.unlock` rule denies everyone (the rule
/// body is written by `write_access_hooks`).
fn users_with_unlock_denied() -> CollectionDefinition {
    let mut def = make_users_def();
    def.access.unlock = Some("hooks.access.deny".into());
    def
}

fn session_version(app: &TestApp, user_id: &str) -> u64 {
    let conn = app.pool.get().unwrap();
    query::get_session_version(&conn, "users", user_id).unwrap()
}

fn is_locked(app: &TestApp, user_id: &str) -> bool {
    let conn = app.pool.get().unwrap();
    query::is_locked(&conn, "users", user_id).unwrap()
}

/// Submit the users edit form for `target_id` as the cookie's user.
async fn save_user(app: &TestApp, cookie: &str, target_id: &str, body: &'static str) -> StatusCode {
    app.router
        .clone()
        .oneshot(
            Request::post(format!("/admin/collections/users/{target_id}"))
                .header("cookie", auth_and_csrf(cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// The rendered edit form of `target_id`, for asserting what persisted.
async fn edit_form_body(app: &TestApp, cookie: &str, target_id: &str) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/users/{target_id}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    body_string(resp.into_body()).await
}

#[tokio::test]
async fn create_form_auth_collection_includes_password() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "authform@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "authform@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/users/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("password"),
        "Auth collection create form should have password field"
    );
}

#[tokio::test]
async fn edit_form_auth_collection_includes_password() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "authedit@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "authedit@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/users/{user_id}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("password"),
        "Auth collection edit form should have password field"
    );
}

#[tokio::test]
async fn create_action_auth_collection_with_password() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/users")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "email=newuser@test.com&name=New+User&password=secret456",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Create auth collection user should succeed, got {status}"
    );
}

/// Regression: the admin save ran the lock/unlock account action on every
/// submit, so a plain edit needed `access.unlock` even though the box was
/// untouched — and the 403 came after the document write had committed.
#[tokio::test]
async fn saving_a_user_without_touching_the_lock_needs_no_unlock_access() {
    let app = setup_app(vec![users_with_unlock_denied()], vec![]);
    write_access_hooks(
        app._tmp.path(),
        "function M.deny(ctx)\n    return false\nend",
    );
    let admin_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &admin_id, "admin@test.com");
    let target_id = create_test_user(&app, "target@test.com", "pass123");
    let version_before = session_version(&app, &target_id);

    let status = save_user(
        &app,
        &cookie,
        &target_id,
        "email=target@test.com&name=Renamed",
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "an edit that leaves the lock alone needs no unlock access"
    );
    assert_eq!(
        session_version(&app, &target_id),
        version_before,
        "no account action ran for an unchanged lock"
    );
    assert!(!is_locked(&app, &target_id));

    let form = edit_form_body(&app, &cookie, &target_id).await;
    assert!(form.contains("Renamed"), "the edit persisted");
}

/// A denied lock toggle is a clean 403: the document write never runs, so
/// nothing the form carried is persisted.
#[tokio::test]
async fn flipping_the_lock_without_unlock_access_is_refused_before_the_write() {
    let app = setup_app(vec![users_with_unlock_denied()], vec![]);
    write_access_hooks(
        app._tmp.path(),
        "function M.deny(ctx)\n    return false\nend",
    );
    let admin_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &admin_id, "admin@test.com");
    let target_id = create_test_user(&app, "target@test.com", "pass123");
    let version_before = session_version(&app, &target_id);

    let status = save_user(
        &app,
        &cookie,
        &target_id,
        "email=target@test.com&name=Renamed&_locked=on",
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        !is_locked(&app, &target_id),
        "the denied lock did not apply"
    );
    assert_eq!(session_version(&app, &target_id), version_before);

    let form = edit_form_body(&app, &cookie, &target_id).await;
    assert!(
        !form.contains("Renamed"),
        "a denied save must persist nothing"
    );
}

/// Regression: re-saving an already locked user re-ran the lock action and
/// bumped the target's `_session_version` on every save.
#[tokio::test]
async fn re_saving_a_locked_user_does_not_bump_the_session_version() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let admin_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &admin_id, "admin@test.com");
    let target_id = create_test_user(&app, "target@test.com", "pass123");

    {
        let conn = app.pool.get().unwrap();
        let ctx = ServiceContext::slug_only("users").conn(&conn).build();
        lock_user(&ctx, &target_id).unwrap();
    }
    let version_before = session_version(&app, &target_id);

    let status = save_user(
        &app,
        &cookie,
        &target_id,
        "email=target@test.com&name=Still+Locked&_locked=on",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(is_locked(&app, &target_id), "the lock stays");
    assert_eq!(
        session_version(&app, &target_id),
        version_before,
        "an unchanged lock is not re-applied"
    );
}

/// The diff still applies a real change: ticking the box locks the user and
/// retires the sessions they hold.
/// A lock toggle rides on the document save: when the save itself fails —
/// here a unique-email clash — the account must not be locked on its own,
/// and its sessions must stay valid.
#[tokio::test]
async fn a_failed_save_does_not_apply_the_lock_toggle() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let admin_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &admin_id, "admin@test.com");
    let target_id = create_test_user(&app, "target@test.com", "pass123");
    create_test_user(&app, "taken@test.com", "pass123");
    let version_before = session_version(&app, &target_id);

    let status = save_user(
        &app,
        &cookie,
        &target_id,
        "email=taken@test.com&name=Locked&_locked=on",
    )
    .await;

    assert_ne!(
        status,
        StatusCode::SEE_OTHER,
        "the clashing save must not succeed"
    );
    assert!(
        !is_locked(&app, &target_id),
        "a failed save must not lock the account"
    );
    assert_eq!(
        session_version(&app, &target_id),
        version_before,
        "a failed save must not retire the user's sessions"
    );
}

#[tokio::test]
async fn ticking_the_lock_box_locks_the_user() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let admin_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &admin_id, "admin@test.com");
    let target_id = create_test_user(&app, "target@test.com", "pass123");
    let version_before = session_version(&app, &target_id);

    let status = save_user(
        &app,
        &cookie,
        &target_id,
        "email=target@test.com&name=Locked&_locked=on",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(is_locked(&app, &target_id));
    assert_eq!(
        session_version(&app, &target_id),
        version_before + 1,
        "locking retires the sessions the user holds"
    );
}
