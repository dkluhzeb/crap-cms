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

//! Conditional field rendering — server-side evaluation via Lua hook.
//! `/admin/collections/{slug}/evaluate-conditions` takes the current form
//! state plus a map of field-name → condition-fn-ref, and returns a map
//! of field-name → visibility bool. The admin UI uses this to show/hide
//! fields as the user types. This file exercises the round-trip with a
//! real Lua condition registered via `hooks/conditions/*.lua`.

use std::fs;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crap_cms::config::CrapConfig;
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms_e2e::helpers::*;

// ── show_when_visible_true_returns_visible ───────────────────────────────

#[tokio::test]
async fn show_when_visible_true_returns_visible() {
    let app = setup_with_condition_hook();
    let user_id = create_test_user(&app, "cond1@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cond1@test.com");

    let body = json!({
        "form_data": { "online": true, "url": "" },
        "conditions": { "url": "hooks.conditions.show_when_online" }
    })
    .to_string();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/events/evaluate-conditions")
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let parsed: Value = serde_json::from_str(&body_string(resp.into_body()).await).unwrap();
    assert_eq!(
        parsed["url"],
        json!(true),
        "url field should be visible when online=true"
    );
}

// ── show_when_visible_false_returns_hidden ───────────────────────────────

#[tokio::test]
async fn show_when_visible_false_returns_hidden() {
    let app = setup_with_condition_hook();
    let user_id = create_test_user(&app, "cond2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cond2@test.com");

    let body = json!({
        "form_data": { "online": false, "url": "" },
        "conditions": { "url": "hooks.conditions.show_when_online" }
    })
    .to_string();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/events/evaluate-conditions")
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let parsed: Value = serde_json::from_str(&body_string(resp.into_body()).await).unwrap();
    assert_eq!(
        parsed["url"],
        json!(false),
        "url field should be hidden when online=false"
    );
}

// ── unknown_condition_ref_rejected ───────────────────────────────────────
//
// Security gate: the handler validates each condition ref against the set
// of refs declared in the collection's field defs. Calling an arbitrary
// Lua function is rejected (treated as "visible" to fail open rather than
// silently hiding fields).

#[tokio::test]
async fn unknown_condition_ref_treated_as_visible() {
    let app = setup_with_condition_hook();
    let user_id = create_test_user(&app, "cond3@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cond3@test.com");

    let body = json!({
        "form_data": { "online": false, "url": "" },
        "conditions": { "url": "hooks.something.totally_unregistered" }
    })
    .to_string();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/events/evaluate-conditions")
                .header("Cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let parsed: Value = serde_json::from_str(&body_string(resp.into_body()).await).unwrap();
    assert_eq!(
        parsed["url"],
        json!(true),
        "unknown condition refs must fail open (visible), not silently hide"
    );
}

fn setup_with_condition_hook() -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");

    // hooks/conditions/show_when_online.lua — file-per-hook style.
    let hooks_dir = tmp.path().join("hooks").join("conditions");
    fs::create_dir_all(&hooks_dir).expect("mkdir hooks/conditions");
    fs::write(
        hooks_dir.join("show_when_online.lua"),
        r"
return function(ctx)
    return ctx.online == true
end
",
    )
    .expect("write hook file");

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.admin.dev_mode = true;

    setup_app_at(
        vec![make_events_def(), make_users_def()],
        vec![],
        config,
        tmp,
    )
}

fn make_events_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("events");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Event".to_string())),
        plural: Some(LocalizedString::Plain("Events".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("online", FieldType::Checkbox).build(),
        FieldDefinition::builder("url", FieldType::Text)
            .admin(
                FieldAdmin::builder()
                    .label(LocalizedString::Plain("Event URL".to_string()))
                    .condition("hooks.conditions.show_when_online")
                    .build(),
            )
            .build(),
    ];
    def
}
