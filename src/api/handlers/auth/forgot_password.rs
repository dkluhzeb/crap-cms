//! Forgot password handler — generate reset token and queue email.

use tokio::task;
use tonic::{Request, Response};
use tracing::error;

use crate::core::collection::Auth;
use crate::{
    api::{content, handlers::ContentService},
    config::{EmailConfig, ServerConfig},
    core::{
        CollectionDefinition, email,
        email::{EmailRenderer, PasswordResetEmailContext},
    },
    db::DbPool,
    service::{ServiceContext, auth::generate_reset_token},
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

        if self.forgot_password_limiter.is_blocked(&req.email)
            || self.ip_forgot_password_limiter.is_blocked(&ip)
        {
            return ok_response;
        }

        self.forgot_password_limiter.record_failure(&req.email);
        self.ip_forgot_password_limiter.record_failure(&ip);

        let Ok(def) = self.get_collection_def(&req.collection) else {
            return ok_response;
        };

        if !def.is_auth_collection()
            || !def.auth.as_ref().is_some_and(Auth::forgot_password_enabled)
            || !def.auth.as_ref().is_some_and(Auth::password_login_enabled)
        {
            return ok_response;
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let user_email = req.email.clone();
        let def_owned = def;
        let email_config = self.email_config.clone();
        let email_renderer = self.email_renderer.clone();
        let server_config = self.server_config.clone();
        let reset_expiry = self.reset_token_expiry;
        let email_max_attempts = self.email_max_attempts;

        task::spawn_blocking(move || {
            send_reset_email(&ResetEmailCtx {
                pool: &pool,
                slug: &slug,
                def: &def_owned,
                user_email: &user_email,
                email_config: &email_config,
                email_renderer: &email_renderer,
                server_config: &server_config,
                reset_expiry,
                email_max_attempts,
            });
        });

        Response::new(content::ForgotPasswordResponse {})
    }
}

/// Context for sending a password reset email.
struct ResetEmailCtx<'a> {
    pool: &'a DbPool,
    slug: &'a str,
    def: &'a CollectionDefinition,
    user_email: &'a str,
    email_config: &'a EmailConfig,
    email_renderer: &'a EmailRenderer,
    server_config: &'a ServerConfig,
    reset_expiry: u64,
    email_max_attempts: u32,
}

/// Generate a reset token, store it, and queue the reset email.
fn send_reset_email(ctx: &ResetEmailCtx) {
    let conn = match ctx.pool.get() {
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

    let base_url = ctx.server_config.public_url.clone().unwrap_or_else(|| {
        if ctx.server_config.host == "0.0.0.0" {
            format!("http://localhost:{}", ctx.server_config.admin_port)
        } else {
            format!(
                "http://{}:{}",
                ctx.server_config.host, ctx.server_config.admin_port
            )
        }
    });

    let reset_url = format!("{base_url}/admin/reset-password?token={token}");

    let html = match ctx.email_renderer.render(
        "password_reset",
        &PasswordResetEmailContext {
            reset_url: &reset_url,
            expiry_minutes: ctx.reset_expiry / 60,
            from_name: &ctx.email_config.from_name,
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
        ctx.email_max_attempts,
    ) {
        error!("Failed to queue reset email: {}", e);
    }
}
