//! Forgot password handler — generate reset token and queue email.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response};
use tracing::error;

use crate::core::collection::Auth;
use crate::{
    api::{content, handlers::ContentService},
    core::{CollectionDefinition, email, email::PasswordResetEmailContext, normalize_email},
    service::{AppInfra, ServiceContext, auth::generate_reset_token},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Initiate a password reset flow -- generates a token and sends a reset email.
    /// Always returns success to prevent leaking user existence.
    pub(in crate::api::handlers) fn forgot_password_impl(
        &self,
        request: Request<content::ForgotPasswordRequest>,
    ) -> Response<content::ForgotPasswordResponse> {
        let ip = request
            .remote_addr()
            .map_or_else(|| "unknown".to_string(), |a| a.ip().to_string());
        let req = request.into_inner();

        let ok_response = Response::new(content::ForgotPasswordResponse {});

        // Normalize the per-email limiter key (trim + lowercase) so casing/
        // whitespace variants of one account share a bucket — `find_by_email` is
        // case-insensitive, so a raw-email key would let an attacker sidestep the
        // per-account reset-flood limit by rotating the spelling.
        let email_key = normalize_email(&req.email);

        // Atomically record this attempt against both limiters and bail if
        // either is now over threshold — one operation per limiter, closing the
        // concurrent-bypass race the is_blocked + separate record split left
        // open. Both are evaluated (not short-circuited) so each counter
        // advances. The generic success response leaks nothing on a block.
        let email_blocked = self.forgot_password_limiter.check_and_block(&email_key);
        let ip_blocked = self.ip_forgot_password_limiter.check_and_block(&ip);
        if email_blocked || ip_blocked {
            return ok_response;
        }

        let Ok(def) = self.get_collection_def(&req.collection) else {
            return ok_response;
        };

        if !def.is_auth_collection()
            || !def.auth.as_ref().is_some_and(Auth::forgot_password_enabled)
            || !def.auth.as_ref().is_some_and(Auth::password_login_enabled)
        {
            return ok_response;
        }

        let infra = Arc::clone(&self.infra);
        let slug = req.collection.clone();
        let user_email = req.email.clone();
        let def_owned = def;
        let reset_expiry = self.reset_token_expiry;

        task::spawn_blocking(move || {
            send_reset_email(&ResetEmailCtx {
                infra: &infra,
                slug: &slug,
                def: &def_owned,
                user_email: &user_email,
                reset_expiry,
            });
        });

        Response::new(content::ForgotPasswordResponse {})
    }
}

/// Context for sending a password reset email. Process-stable dependencies
/// (pool + email config/renderer/server config) come from the shared
/// [`AppInfra`]; `reset_expiry` and the target collection are per-call.
struct ResetEmailCtx<'a> {
    infra: &'a AppInfra,
    slug: &'a str,
    def: &'a CollectionDefinition,
    user_email: &'a str,
    reset_expiry: u64,
}

/// Generate a reset token, store it, and queue the reset email.
fn send_reset_email(ctx: &ResetEmailCtx) {
    let conn = match ctx.infra.pool.get() {
        Ok(c) => c,
        Err(e) => {
            error!("DB connection for forgot password: {}", e);
            return;
        }
    };

    let svc_ctx = ServiceContext::collection(ctx.slug, ctx.def)
        .conn(&conn)
        .build();

    let token_result = match generate_reset_token(&svc_ctx, ctx.user_email, ctx.reset_expiry) {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            error!("Forgot password error: {}", e);
            return;
        }
    };
    let token = &token_result.token;

    // Use the shared `base_url()` so a configured `public_url` with a trailing
    // slash is trimmed — the hand-rolled form produced `…com//admin/…`.
    let base_url = ctx.infra.email.server_config.base_url();

    let reset_url = format!("{base_url}/admin/reset-password?token={token}");

    let html = match ctx.infra.email.email_renderer.render(
        "password_reset",
        &PasswordResetEmailContext {
            reset_url: &reset_url,
            expiry_minutes: ctx.reset_expiry / 60,
            from_name: &ctx.infra.email.email_config.from_name,
        },
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to render reset email: {}", e);
            return;
        }
    };

    if let Err(e) = email::queue_email(
        &conn,
        &email::EmailJobData {
            to: ctx.user_email.to_string(),
            subject: "Reset your password".to_string(),
            html,
            text: None,
        },
        ctx.infra.email.email_max_attempts,
    ) {
        error!("Failed to queue reset email: {}", e);
    }
}
