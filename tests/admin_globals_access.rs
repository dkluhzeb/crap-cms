//! Global access control on the admin HTTP surface.
//!
//! Service-layer coverage lives in `tests/hook_lifecycle_globals.rs`; these
//! tests assert the translation: a `ServiceError::AccessDenied` returned by the
//! service layer becomes a `403` on the admin HTTP surface (both GET and POST
//! handlers).
//!
//! Uses a Lua fixture under `tests/fixtures/admin_globals_access/` so the
//! registry is built from real `crap.globals.define` + `crap.collections.define`
//! calls and the access hook is a real Lua function.

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

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use admin_globals_support::{
    TEST_CSRF, auth_and_csrf, create_test_user_with_role, make_auth_cookie, setup_app_with_fixture,
};
use crap_cms::{
    config::CrapConfig,
    core::Document,
    db::{DbConnection, DbValue, migrate, pool, query},
    hooks,
    hooks::lifecycle::HookRunner,
    service::{GetGlobalInput, RunnerReadHooks, ServiceContext, get_global_document},
};

fn admin_globals_access_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/admin_globals_access")
}

#[tokio::test]
async fn global_read_access_denied_returns_403_admin() {
    let app = setup_app_with_fixture(&admin_globals_access_fixture());
    let user_id = create_test_user_with_role(&app, "editor@test.com", "pass123", Some("editor"));
    let cookie = make_auth_cookie(&app, &user_id, "editor@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/restricted_settings")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "non-admin read should be 403"
    );
}

#[tokio::test]
async fn global_update_access_denied_returns_403_admin() {
    let app = setup_app_with_fixture(&admin_globals_access_fixture());
    let user_id = create_test_user_with_role(&app, "editor2@test.com", "pass123", Some("editor"));
    let cookie = make_auth_cookie(&app, &user_id, "editor2@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/restricted_settings")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("secret_value=Hacked"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "non-admin update should be 403"
    );
}

/// Service-layer control: verify the fixture's `admin_only` hook correctly allows
/// a role=admin user when we skip the HTTP middleware path entirely. This
/// isolates whether the bug is in the HTTP chain (`auth_middleware` /
/// `load_auth_user`) or the service/access layer.
#[test]
fn global_read_admin_via_service_layer_allowed() {
    let fixture = admin_globals_access_fixture();
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let registry = hooks::init_lua(&fixture, &config).expect("init lua");
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("migrate");

    let runner = HookRunner::builder()
        .config_dir(&fixture)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");

    let def = registry.get_global("restricted_settings").unwrap().clone();

    let mut admin_fields = HashMap::new();
    admin_fields.insert("role".to_string(), json!("admin"));
    admin_fields.insert("email".to_string(), json!("admin@test.com"));
    let admin = Document {
        id: "admin-1".into(),
        fields: admin_fields.into(),
        created_at: None,
        updated_at: None,
    };

    let conn = db_pool.get().unwrap();
    let rh = RunnerReadHooks::new(&runner, &conn, None, None);
    let ctx = ServiceContext::global("restricted_settings", &def)
        .conn(&conn)
        .read_hooks(&rh)
        .user(Some(&admin))
        .build();

    let input = GetGlobalInput::new(None, None);
    get_global_document(&ctx, &input)
        .expect("admin role should be allowed through the admin_only hook");
}

#[tokio::test]
async fn global_read_access_allowed_for_admin() {
    let app = setup_app_with_fixture(&admin_globals_access_fixture());
    let user_id = create_test_user_with_role(&app, "admin@test.com", "pass123", Some("admin"));

    // Verify the role was actually stored in the DB.
    {
        let conn = app.pool.get().unwrap();
        let row = conn
            .query_one(
                "SELECT email, role FROM users WHERE id = ?1",
                &[DbValue::Text(user_id.clone())],
            )
            .unwrap()
            .expect("user row must exist");
        let role = row.get_opt_string("role").unwrap();
        assert_eq!(
            role.as_deref(),
            Some("admin"),
            "DB sanity: role column must be 'admin', got {role:?}"
        );
    }

    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/globals/restricted_settings")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body_bytes = to_bytes(resp.into_body(), 1_000_000).await.unwrap();
    let body = String::from_utf8_lossy(&body_bytes);
    assert_eq!(
        status,
        StatusCode::OK,
        "admin read should be 200; body was: {body}"
    );
}

/// Regression: the restore confirmation page read a filter table returned by
/// a global's `update` access as "allowed" and rendered, though the restore
/// itself (like every global surface) refuses it as a configuration error.
#[tokio::test]
async fn global_restore_confirm_refuses_a_filter_table_update_rule() {
    let app = setup_app_with_fixture(&admin_globals_access_fixture());
    let user_id = create_test_user_with_role(&app, "scoped@test.com", "pass123", Some("admin"));
    let cookie = make_auth_cookie(&app, &user_id, "scoped@test.com");

    let version = {
        let conn = app.pool.get().unwrap();
        query::create_version(
            &conn,
            "_global_scoped_settings",
            "default",
            "published",
            &json!({ "motto": "Old" }),
        )
        .expect("record a version")
    };

    let resp = app
        .router
        .oneshot(
            Request::get(format!(
                "/admin/globals/scoped_settings/versions/{}/restore",
                version.id
            ))
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// Regression: display-condition evaluation read a filter table returned by
/// a global's `read` access as "allowed".
#[tokio::test]
async fn global_evaluate_conditions_refuses_a_filter_table_read_rule() {
    let app = setup_app_with_fixture(&admin_globals_access_fixture());
    let user_id = create_test_user_with_role(&app, "scoped2@test.com", "pass123", Some("admin"));
    let cookie = make_auth_cookie(&app, &user_id, "scoped2@test.com");
    let body = json!({ "form_data": { "motto": "x" }, "conditions": {} });

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/globals/scoped_read_settings/evaluate-conditions")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
