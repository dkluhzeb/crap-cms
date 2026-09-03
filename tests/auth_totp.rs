//! Integration tests for `mfa = "totp"`: challenge-driven enrollment and the
//! mode-dispatching second-factor verification chokepoint
//! (`service::auth::verify_second_factor`).

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

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crap_cms::config::CrapConfig;
use crap_cms::core::auth::totp_code_at;
use crap_cms::core::collection::{Auth, CollectionDefinition, MfaMode};
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::{Document, DocumentFields, Registry};
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{self, AppInfra, ServiceContext, StandaloneInfra, auth};

const SECRET: &str = "test-auth-secret";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn users_def(mode: MfaMode) -> CollectionDefinition {
    let mut def = CollectionDefinition::new("users");
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .build(),
    ];
    def.auth = Some(Auth::enabled().map_password_login(|b| b.mfa(mode)));
    def
}

fn setup(mode: MfaMode) -> (tempfile::TempDir, Arc<AppInfra>, Document) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = SECRET.into();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let shared = Registry::shared();
    shared.write().unwrap().register_collection(users_def(mode));
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");
    let storage = crap_cms::core::upload::create_storage(
        tmp.path(),
        &crap_cms::config::UploadConfig::default(),
    )
    .unwrap();

    let user = {
        let conn = db_pool.get().unwrap();
        let def = registry.get_collection("users").unwrap().clone();
        let fields: DocumentFields = [("email".to_string(), json!("totp@test.com"))]
            .into_iter()
            .collect();
        query::create(&conn, "users", &def, &fields, None).expect("create user")
    };

    let infra = AppInfra::standalone(StandaloneInfra {
        pool: db_pool,
        registry,
        hook_runner: runner,
        storage,
        token_provider: None,
        event_transport: None,
        invalidation_transport: None,
        config: &config,
        config_dir: tmp.path(),
    })
    .expect("infra");

    (tmp, infra, user)
}

/// The full enrollment arc: provision → verify (confirm) → no more
/// provisioning → replay rejected → next step accepted.
#[test]
fn enrollment_and_verification_flow() {
    let (_tmp, infra, user) = setup(MfaMode::Totp);
    let uid = user.id.to_string();

    // First challenge provisions.
    let prov = auth::totp_challenge(&infra, SECRET, "users", &user)
        .expect("challenge")
        .expect("unenrolled user gets provisioning");
    assert!(prov.uri.starts_with("otpauth://totp/"), "{}", prov.uri);
    assert!(prov.uri.contains(&prov.secret));

    // The stored secret is sealed, never plaintext.
    {
        let conn = infra.pool.get().unwrap();
        let state = query::get_totp_state(&conn, "users", &uid)
            .unwrap()
            .unwrap();
        assert_ne!(state.sealed_secret.as_deref(), Some(prov.secret.as_str()));
        assert!(!state.confirmed);
    }

    // A repeated challenge re-shows the SAME enrollment (resumable).
    let again = auth::totp_challenge(&infra, SECRET, "users", &user)
        .unwrap()
        .expect("still unenrolled");
    assert_eq!(again.secret, prov.secret);

    // Wrong code fails; the right code confirms.
    assert!(!auth::verify_second_factor(&infra, SECRET, "users", &uid, "000000").unwrap());

    let t = now();
    let code = totp_code_at(&prov.secret, t).unwrap();
    assert!(auth::verify_second_factor(&infra, SECRET, "users", &uid, &code).unwrap());

    // Confirmed: no more provisioning, and the same code never replays.
    assert!(
        auth::totp_challenge(&infra, SECRET, "users", &user)
            .unwrap()
            .is_none(),
        "confirmed enrollment must not re-provision"
    );
    assert!(
        !auth::verify_second_factor(&infra, SECRET, "users", &uid, &code).unwrap(),
        "a code must never verify twice"
    );

    // The next time step's code works (within the ±1 window).
    let next = totp_code_at(&prov.secret, t + 30).unwrap();
    assert!(auth::verify_second_factor(&infra, SECRET, "users", &uid, &next).unwrap());
}

/// The chokepoint dispatches `email` mode to the stored-code path.
#[test]
fn email_mode_dispatches_to_stored_code() {
    let (_tmp, infra, user) = setup(MfaMode::Email);
    let uid = user.id.to_string();

    {
        let conn = infra.pool.get().unwrap();
        let ctx = ServiceContext::slug_only("users").conn(&conn).build();
        service::auth::set_mfa_code(&ctx, &uid, "123456", now() + 300).unwrap();
    }

    assert!(auth::verify_second_factor(&infra, SECRET, "users", &uid, "123456").unwrap());
    // Single-use: cleared after the first verify.
    assert!(!auth::verify_second_factor(&infra, SECRET, "users", &uid, "123456").unwrap());
}

/// A rotated `[auth] secret` makes the sealed secret unopenable:
/// verification fails closed, and the next challenge restarts enrollment.
#[test]
fn rotated_secret_restarts_enrollment() {
    let (_tmp, infra, user) = setup(MfaMode::Totp);
    let uid = user.id.to_string();

    let prov = auth::totp_challenge(&infra, SECRET, "users", &user)
        .unwrap()
        .unwrap();
    let code = totp_code_at(&prov.secret, now()).unwrap();
    assert!(auth::verify_second_factor(&infra, SECRET, "users", &uid, &code).unwrap());

    // With a rotated secret nothing verifies…
    let next = totp_code_at(&prov.secret, now() + 30).unwrap();
    assert!(!auth::verify_second_factor(&infra, "rotated", "users", &uid, &next).unwrap());

    // …and the next challenge restarts enrollment with a fresh secret.
    let restarted = auth::totp_challenge(&infra, "rotated", "users", &user)
        .unwrap()
        .expect("rotation must restart enrollment");
    assert_ne!(restarted.secret, prov.secret);

    let conn = infra.pool.get().unwrap();
    let state = query::get_totp_state(&conn, "users", &uid)
        .unwrap()
        .unwrap();
    assert!(!state.confirmed, "restart resets the confirmed flag");
}
