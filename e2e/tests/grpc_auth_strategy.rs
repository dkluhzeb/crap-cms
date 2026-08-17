//! gRPC e2e: custom Lua `authenticate` strategies.
//!
//! Auth strategies let a collection plug in non-password
//! authentication: the Lua function receives `{ headers,
//! collection }` and returns a user document (or nil to defer).
//! `service::auth::login` tries them after email/password fails.
//!
//! For the gRPC Login path the headers map is always empty (the
//! handler doesn't surface metadata to strategies — a real
//! limitation worth knowing about), so the strategy can only
//! authenticate based on collection + DB lookup. The tests below
//! pin:
//!   - happy path: a strategy that returns a user → Login succeeds
//!     even with the wrong password
//!   - deny path: a strategy that always returns nil → Login still
//!     fails when the password is wrong
//!   - non-interference: a passing strategy must not break the
//!     normal email/password path

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

use std::collections::HashMap;

use crap_cms::api::content::{DataMap, FieldValue, field_value::Kind};

use crap_cms::{
    api::content::{CreateRequest, LoginRequest, content_api_client::ContentApiClient},
    core::{
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
    },
};
use crap_cms_e2e::spawn_grpc_server_with_lua;

// ── Lua fixtures ─────────────────────────────────────────────────────────

/// Strategy that authenticates the first user in the collection as
/// itself. Equivalent to "single-user auto-login fallback" — useful
/// for verifying the strategy mechanism is wired through to Login
/// over the wire. Uses `crap.collections.find_by_id` only after a
/// `find` to locate the doc; returns the doc table verbatim so
/// `lua_table_to_auth_user` can read `id` plus any field values.
const ANY_USER_STRATEGY: &str = r"
local M = {}
function M.authenticate(ctx)
    local result = crap.collections.find(ctx.collection, { limit = 1 })
    if not result or not result.documents or #result.documents == 0 then
        return nil
    end
    return result.documents[1]
end
return M
";

/// Strategy that always returns nil — never authenticates.
const ALWAYS_NIL_STRATEGY: &str = r"
local M = {}
function M.authenticate(_ctx)
    return nil
end
return M
";

// ── helpers ──────────────────────────────────────────────────────────────

fn proto_struct(pairs: &[(&str, &str)]) -> DataMap {
    let mut fields = HashMap::new();
    for (k, v) in pairs {
        fields.insert(
            (*k).to_string(),
            FieldValue {
                kind: Some(Kind::StringValue((*v).to_string())),
            },
        );
    }
    DataMap { fields }
}

fn users_def_with_strategy(name: &str, authenticate: &str) -> CollectionDefinition {
    let mut def = CollectionDefinition::new("users");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("User".to_string())),
        plural: Some(LocalizedString::Plain("Users".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
        FieldDefinition::builder("name", FieldType::Text).build(),
    ];
    let mut auth = Auth::enabled();
    auth.methods.push(AuthMethod::Strategy {
        name: name.to_string(),
        authenticate: authenticate.into(),
        activates_on: Activation::always(),
        surfaces: SurfaceSet::admin_only(),
    });
    def.auth = Some(auth);
    def
}

// ── strategy_authenticates_when_password_is_wrong ────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn strategy_authenticates_when_password_is_wrong() {
    let def = users_def_with_strategy("any-user", "hooks.any_user.authenticate");

    let ctx = spawn_grpc_server_with_lua(
        vec![def],
        vec![],
        &[("hooks/any_user.lua", ANY_USER_STRATEGY)],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    // Seed a single user so the strategy has something to return.
    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "real@x.com"),
                ("name", "Real User"),
                ("password", "real-password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user");

    // Login with the WRONG password — local auth fails, strategy
    // fallback authenticates the user and returns a token.
    let token = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "real@x.com".to_string(),
            password: "wrong-password".to_string(),
        })
        .await
        .expect("login should succeed via strategy fallback")
        .into_inner()
        .token;
    assert!(
        !token.is_empty(),
        "strategy fallback should return a non-empty token"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── always_nil_strategy_does_not_authenticate_wrong_password ─────────────
//
// A strategy that returns nil must not rescue a wrong-password
// login. The local auth still fails, and with no fallback the
// request is denied.

#[tokio::test(flavor = "multi_thread")]
async fn always_nil_strategy_does_not_rescue_wrong_password() {
    let def = users_def_with_strategy("nope", "hooks.nope.authenticate");

    let ctx = spawn_grpc_server_with_lua(
        vec![def],
        vec![],
        &[("hooks/nope.lua", ALWAYS_NIL_STRATEGY)],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "lock@x.com"),
                ("name", "Lock"),
                ("password", "right-password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user");

    let result = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "lock@x.com".to_string(),
            password: "wrong-password".to_string(),
        })
        .await;

    assert!(
        result.is_err(),
        "always-nil strategy should not rescue wrong password"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── correct_password_still_works_alongside_strategy ──────────────────────
//
// Strategies are a *fallback* after local email/password
// authentication — the normal happy path must still work without
// interference.

#[tokio::test(flavor = "multi_thread")]
async fn correct_password_works_alongside_strategy() {
    let def = users_def_with_strategy("any-user", "hooks.any_user.authenticate");

    let ctx = spawn_grpc_server_with_lua(
        vec![def],
        vec![],
        &[("hooks/any_user.lua", ANY_USER_STRATEGY)],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "happy@x.com"),
                ("name", "Happy"),
                ("password", "happy-password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user");

    let token = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "happy@x.com".to_string(),
            password: "happy-password-12345".to_string(),
        })
        .await
        .expect("correct password should still work")
        .into_inner()
        .token;
    assert!(!token.is_empty(), "happy-path login should return a token");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
