//! gRPC e2e: account admin actions (Lock / Unlock / Verify / Unverify).
//!
//! All four `AccountActionRequest`-shaped RPCs share the same
//! `extract_token` → `resolve_auth_user` → call-typed-service-fn
//! plumbing. The handler currently only requires *some*
//! authenticated caller (no admin-role check), which is a known
//! authorization gap — but the e2e contract here is just "does the
//! RPC reach the service layer over the wire and apply the
//! change." Tighter authz tests belong to a future commit that
//! adds role gating.

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

use std::collections::BTreeMap;

use prost_types::{Struct, Value, value::Kind};
use tonic::{Code, Request, metadata::MetadataValue};

use crap_cms::{
    api::content::{
        AccountActionRequest, CreateRequest, LoginRequest, content_api_client::ContentApiClient,
    },
    core::{
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
    },
    db::{DbPool, query},
};
use crap_cms_e2e::spawn_grpc_server;

/// Mark a freshly-created user as verified so Login succeeds on
/// auth collections with `verify_email = true`. Without this, every
/// `create_and_login` would need a real ForgotPassword/ResetPassword
/// round-trip just to bootstrap the account.
fn mark_verified_direct(pool: &DbPool, id: &str) {
    let conn = pool.get().expect("pool");
    query::mark_verified(&conn, "users", id).expect("mark_verified");
}

fn make_users_def() -> CollectionDefinition {
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
    // `verify_email = true` provisions the `_verified` /
    // `_verification_token` / `_verification_token_exp` columns,
    // required by `VerifyAccount` / `UnverifyAccount`. The
    // `verify_account_returns_failed_precondition_without_verify_email`
    // test below exercises the opposite case to pin the preflight
    // check added to those handlers.
    def.auth = Some(Auth::enabled().map_password_login(|b| b.verify_email(true)));
    def
}

fn make_users_def_no_verify_email() -> CollectionDefinition {
    let mut def = make_users_def();
    if let Some(auth) = def.auth.take() {
        def.auth = Some(auth.map_password_login(|b| b.verify_email(false)));
    }
    def
}

fn proto_struct(pairs: &[(&str, &str)]) -> Struct {
    let mut fields = BTreeMap::new();
    for (k, v) in pairs {
        fields.insert(
            (*k).to_string(),
            Value {
                kind: Some(Kind::StringValue((*v).to_string())),
            },
        );
    }
    Struct { fields }
}

/// Create a user, mark as verified (`verify_email = true` on the
/// collection requires this for login), then log in. Returns the
/// user's `(id, login_token)`.
async fn create_and_login(
    client: &mut ContentApiClient<tonic::transport::Channel>,
    pool: &DbPool,
    email: &str,
    password: &str,
) -> (String, String) {
    let id = client
        .create(CreateRequest {
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", email),
                ("name", email),
                ("password", password),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user")
        .into_inner()
        .document
        .expect("doc")
        .id;
    mark_verified_direct(pool, &id);
    let token = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: email.to_string(),
            password: password.to_string(),
        })
        .await
        .expect("login")
        .into_inner()
        .token;
    (id, token)
}

fn with_bearer<T>(req: T, token: &str) -> Request<T> {
    let mut r = Request::new(req);
    let bearer: MetadataValue<_> = format!("Bearer {token}").parse().expect("valid metadata");
    r.metadata_mut().insert("authorization", bearer);
    r
}

// ── lock_then_unlock_account_round_trip ──────────────────────────────────
//
// LockAccount blocks login; UnlockAccount restores it. Verified
// end-to-end by attempting Login between the two actions.

#[tokio::test(flavor = "multi_thread")]
async fn lock_then_unlock_account_round_trip() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let (_admin_id, admin_token) =
        create_and_login(&mut client, &ctx.pool, "admin@x.com", "password-12345").await;
    let (target_id, _target_token) =
        create_and_login(&mut client, &ctx.pool, "target@x.com", "password-12345").await;

    // Lock — Login should now fail with AccountLocked-equivalent code.
    client
        .lock_account(with_bearer(
            AccountActionRequest {
                collection: "users".to_string(),
                id: target_id.clone(),
            },
            &admin_token,
        ))
        .await
        .expect("lock_account");

    let blocked = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "target@x.com".to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect_err("locked user login should fail");
    assert_ne!(blocked.code(), Code::Ok);
    assert_ne!(
        blocked.code(),
        Code::Internal,
        "locked-user login should not surface as Internal, got {:?}: {}",
        blocked.code(),
        blocked.message()
    );

    // Unlock — Login works again.
    client
        .unlock_account(with_bearer(
            AccountActionRequest {
                collection: "users".to_string(),
                id: target_id.clone(),
            },
            &admin_token,
        ))
        .await
        .expect("unlock_account");

    client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "target@x.com".to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect("unlocked user should login again");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── verify_then_unverify_account_round_trip ──────────────────────────────
//
// Toggles `_verified` on the user doc. Both RPCs should succeed and
// be idempotent.

#[tokio::test(flavor = "multi_thread")]
async fn verify_then_unverify_account_round_trip() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let (_admin_id, admin_token) =
        create_and_login(&mut client, &ctx.pool, "admin2@x.com", "password-12345").await;
    let (target_id, _) =
        create_and_login(&mut client, &ctx.pool, "target2@x.com", "password-12345").await;

    client
        .verify_account(with_bearer(
            AccountActionRequest {
                collection: "users".to_string(),
                id: target_id.clone(),
            },
            &admin_token,
        ))
        .await
        .expect("verify_account");

    client
        .unverify_account(with_bearer(
            AccountActionRequest {
                collection: "users".to_string(),
                id: target_id,
            },
            &admin_token,
        ))
        .await
        .expect("unverify_account");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── account_action_without_auth_returns_unauthenticated ──────────────────

#[tokio::test(flavor = "multi_thread")]
async fn account_action_without_auth_returns_unauthenticated() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let (target_id, _) =
        create_and_login(&mut client, &ctx.pool, "victim@x.com", "password-12345").await;

    let status = client
        .lock_account(AccountActionRequest {
            collection: "users".to_string(),
            id: target_id,
        })
        .await
        .expect_err("lock_account without auth should fail");

    assert_eq!(
        status.code(),
        Code::Unauthenticated,
        "no Bearer token → UNAUTHENTICATED, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── verify_account_returns_failed_precondition_without_verify_email ──────
//
// Regression test: calling `VerifyAccount` / `UnverifyAccount` on an
// auth collection without `verify_email = true` used to surface as
// `Status::internal("Internal error")` because the underlying SQL
// touched `_verified` / `_verification_token` columns that aren't
// provisioned for non-verify-email collections. The handler now
// preflight-checks the collection's `auth.verify_email` flag and
// returns `FailedPrecondition` — the correct mapping per gRPC
// status-code semantics (server healthy, request well-formed, but
// the system's state doesn't allow the operation).

#[tokio::test(flavor = "multi_thread")]
async fn verify_account_returns_failed_precondition_without_verify_email() {
    let ctx = spawn_grpc_server(vec![make_users_def_no_verify_email()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    // Create + login an admin caller. No mark_verified needed since
    // verify_email is off on this collection (login is unconditional).
    let create_resp = client
        .create(CreateRequest {
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "preflight@x.com"),
                ("name", "preflight"),
                ("password", "password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create");
    let admin_id = create_resp.into_inner().document.expect("doc").id;
    let admin_token = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "preflight@x.com".to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect("login")
        .into_inner()
        .token;

    let verify_status = client
        .verify_account(with_bearer(
            AccountActionRequest {
                collection: "users".to_string(),
                id: admin_id.clone(),
            },
            &admin_token,
        ))
        .await
        .expect_err("verify_account without verify_email should fail");
    assert_eq!(
        verify_status.code(),
        Code::FailedPrecondition,
        "verify_account → FailedPrecondition when verify_email is off, got {:?}: {}",
        verify_status.code(),
        verify_status.message()
    );

    let unverify_status = client
        .unverify_account(with_bearer(
            AccountActionRequest {
                collection: "users".to_string(),
                id: admin_id,
            },
            &admin_token,
        ))
        .await
        .expect_err("unverify_account without verify_email should fail");
    assert_eq!(
        unverify_status.code(),
        Code::FailedPrecondition,
        "unverify_account → FailedPrecondition when verify_email is off, got {:?}: {}",
        unverify_status.code(),
        unverify_status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
