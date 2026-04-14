//! Forgot password handler — generate reset token and queue email.

use serde_json::json;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    config::{EmailConfig, ServerConfig},
    core::{CollectionDefinition, email, email::EmailRenderer},
    db::DbPool,
    service::{ServiceContext, auth::generate_reset_token},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Initiate a password reset flow -- generates a token and sends a reset email.
    /// Always returns success to prevent leaking user existence.
    pub(in crate::api::handlers) async fn forgot_password_impl(
        &self,
        request: Request<content::ForgotPasswordRequest>,
    ) -> Result<Response<content::ForgotPasswordResponse>, Status> {
        let ip = request
            .remote_addr()
            .map(|a| a.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let req = request.into_inner();

        if self.forgot_password_limiter.is_blocked(&req.email)
            || self.ip_forgot_password_limiter.is_blocked(&ip)
        {
            return Ok(Response::new(content::ForgotPasswordResponse {
                success: true,
            }));
        }

        self.forgot_password_limiter.record_failure(&req.email);
        self.ip_forgot_password_limiter.record_failure(&ip);

        let ok_response = Response::new(content::ForgotPasswordResponse { success: true });

        let def = match self.get_collection_def(&req.collection) {
            Ok(d) => d,
            Err(_) => return Ok(ok_response),
        };

        if !def.is_auth_collection()
            || !def.auth.as_ref().is_some_and(|a| a.forgot_password)
            || def.auth.as_ref().is_some_and(|a| a.disable_local)
        {
            return Ok(ok_response);
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let user_email = req.email.clone();
        let def_owned = def;
        let email_config = self.email_config.clone();
        let email_renderer = self.email_renderer.clone();
        let server_config = self.server_config.clone();
        let reset_expiry = self.reset_token_expiry;

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
            });
        });

        Ok(Response::new(content::ForgotPasswordResponse {
            success: true,
        }))
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

    let reset_url = format!("{}/admin/reset-password?token={}", base_url, token);

    let html = match ctx.email_renderer.render(
        "password_reset",
        &json!({
            "reset_url": reset_url,
            "expiry_minutes": ctx.reset_expiry / 60,
            "from_name": ctx.email_config.from_name,
        }),
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to render reset email: {}", e);
            return;
        }
    };

    if let Err(e) = email::queue_email(
        &conn,
        ctx.user_email,
        "Reset your password",
        &html,
        None,
        ctx.email_config.queue_retries + 1,
        &ctx.email_config.queue_name,
    ) {
        error!("Failed to queue reset email: {}", e);
    }
}
