use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::Response,
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState, auth_middleware,
        handlers::auth::{
            LoginForm, client_ip, create_session_token, issue_mfa_challenge, login_error,
            refusal_error_key, session_redirect,
        },
        server::headers_to_map,
    },
    core::{
        CollectionDefinition, Document, SharedPasswordProvider,
        collection::{Auth, Surface},
        login_email_key,
        rate_limit::AttemptBudget,
    },
    service::{
        AppInfra, ServiceError,
        auth::{LoginFlowRequest, LoginOutcome, LoginVerified, SessionGrant, verify_login},
    },
};

/// Owned bundle for the login spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct VerifyParams {
    infra: Arc<AppInfra>,
    password_provider: SharedPasswordProvider,
    slug: String,
    def: Arc<CollectionDefinition>,
    email: String,
    password: String,
    remote_addr: String,
    headers: HashMap<String, String>,
}

/// Run the shared credential-verification flow
/// ([`verify_login`]) on the blocking pool — the same flow the
/// gRPC login uses, so the two surfaces cannot drift.
async fn verify_credentials(
    params: VerifyParams,
) -> Result<Result<LoginOutcome, ServiceError>, task::JoinError> {
    task::spawn_blocking(move || {
        verify_login(
            &params.infra,
            &LoginFlowRequest {
                slug: &params.slug,
                def: &params.def,
                email: &params.email,
                password: &params.password,
                headers: &params.headers,
                remote_addr: Some(&params.remote_addr),
                surface: Surface::Admin,
                password_provider: &*params.password_provider,
            },
        )
    })
    .await
}

/// The address a session / MFA challenge for `user` carries: the stored
/// email, falling back to the one the login form submitted.
fn login_email(user: &Document, form: &LoginForm) -> String {
    user.fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or(&form.email)
        .to_string()
}

/// Issue the MFA challenge for a password-verified login; a refusal
/// re-renders the login form with its error.
fn handle_mfa_challenge(state: &AdminState, login: &LoginVerified, form: &LoginForm) -> Response {
    let email = login_email(&login.user, form);

    issue_mfa_challenge(state, &form.collection, login, &email)
        .unwrap_or_else(|refusal| login_error(state, refusal_error_key(refusal), &form.email))
}

/// Build the authenticated session response (JWT + cookies + redirect).
fn build_session_response(state: &AdminState, login: &LoginVerified, form: &LoginForm) -> Response {
    let user = &login.user;
    let user_email = login_email(user, form);

    let grant = SessionGrant::builder(
        &user.id,
        &form.collection,
        &user_email,
        login.session_version,
        Surface::Admin,
    )
    .mfa(login.mfa);

    let session = match create_session_token(state, grant) {
        Ok(s) => s,
        Err(e) => {
            error!("{}", e);
            return login_error(state, "error_internal", &form.email);
        }
    };

    session_redirect(state, &session)
}

/// POST /admin/login — verify credentials, set cookie, redirect.
pub async fn login_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let client = client_ip(&headers, &addr, &state.config.server);

    // An address longer than any deliverable one cannot name an account: it
    // is refused before it is keyed, throttled, looked up or echoed back.
    // Otherwise the per-email limiter is keyed on the address in its stored
    // form (trimmed, lowercased, NFC-composed), so spelling variants of one
    // account share a bucket — the credential lookup compares that form.
    let Some(email_key) = login_email_key(&form.email) else {
        return login_error(&state, "error_invalid_credentials", "");
    };

    // Atomically record this attempt, IP budget first (see `AttemptBudget`),
    // and reject if either budget is spent. A successful login settles both
    // below.
    let budget = AttemptBudget::new(&state.ip_login_limiter, &state.login_limiter);
    if budget.check_and_block(&client, &email_key) {
        return login_error(&state, "error_too_many_attempts", &form.email);
    }

    let Some(def) = state
        .infra
        .registry
        .get_collection(&form.collection)
        .cloned()
        .filter(|d| d.is_auth_collection())
    else {
        return login_error(&state, "error_invalid_collection", &form.email);
    };

    let allows_password = def.auth.as_ref().is_some_and(Auth::password_login_enabled);
    let has_strategies = def.auth.as_ref().is_some_and(Auth::has_strategies);

    // If password login is off and no strategies, nothing can authenticate
    if !allows_password && !has_strategies {
        return login_error(&state, "error_invalid_collection", &form.email);
    }

    let result = verify_credentials(VerifyParams {
        infra: state.infra.clone(),
        password_provider: state.password_provider.clone(),
        slug: form.collection.clone(),
        def: def.clone(),
        email: form.email.clone(),
        password: form.password.clone(),
        remote_addr: client.to_string(),
        headers: headers_to_map(&headers),
    })
    .await;

    let outcome = match result {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => {
            error!("Login error: {}", e);

            return login_error(&state, "error_internal", &form.email);
        }
        Err(e) => {
            error!("Login task error: {}", e);

            return login_error(&state, "error_internal", &form.email);
        }
    };

    let (login, mfa_required) = match outcome {
        LoginOutcome::Verified(v) => (v, false),
        LoginOutcome::MfaRequired(v) => (v, true),
        LoginOutcome::Denied => {
            return login_error(&state, "error_invalid_credentials", &form.email);
        }
    };

    // Successful login: the account proved its identity, so its per-email
    // budget is cleared; the SHARED per-IP budget only gets this attempt
    // refunded, so one valid account on a shared IP cannot mask a brute force
    // of others from behind it.
    budget.settle_success(&client, &email_key);

    // Check admin.access gate before issuing session — deny login entirely
    // if the user doesn't pass the gate function.
    if let Some(response) = auth_middleware::check_admin_gate_for_doc(&state, &login.user).await {
        return response;
    }

    // MFA requirement is decided inside the shared flow.
    if mfa_required {
        return handle_mfa_challenge(&state, &login, &form);
    }

    build_session_response(&state, &login, &form)
}
