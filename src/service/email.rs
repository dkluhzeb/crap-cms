//! Fire-and-forget email operations (verification emails).

use std::sync::Arc;

use chrono::Utc;
use nanoid::nanoid;
use tracing::{error, warn};

use crate::{
    config::{EmailConfig, ServerConfig},
    core::email::{EmailJobData, EmailRenderer, VerifyEmailContext, is_configured, queue_email},
    db::{DbPool, query},
};

/// Inputs for [`send_verification_email`]. All fields are required;
/// every call site builds this once and consumes it, so a plain
/// struct literal is enough — no builder.
pub(crate) struct VerificationEmailInput {
    pub pool: DbPool,
    pub email_config: EmailConfig,
    pub email_renderer: Arc<EmailRenderer>,
    pub server_config: ServerConfig,
    pub email_max_attempts: u32,
    pub slug: String,
    pub user_id: String,
    pub user_email: String,
}

/// Generate a verification token and send the verification email.
/// Spawns its own `spawn_blocking` task internally.
// Excluded from coverage: async tokio task that requires SMTP email transport,
// DB pool, and email renderer — cannot be unit tested without external services.
#[cfg(not(tarpaulin_include))]
pub(crate) fn send_verification_email(input: VerificationEmailInput) {
    let VerificationEmailInput {
        pool,
        email_config,
        email_renderer,
        server_config,
        email_max_attempts,
        slug,
        user_id,
        user_email,
    } = input;

    tokio::task::spawn_blocking(move || {
        if !is_configured(&email_config) {
            warn!(
                "Email not configured — skipping verification email for {}",
                user_email
            );

            return;
        }

        let token = nanoid!(32);
        let exp = Utc::now().timestamp() + 86400; // 24 hours

        let conn = match pool.get() {
            Ok(c) => c,
            Err(e) => {
                error!("DB connection for verification token: {}", e);

                return;
            }
        };

        if let Err(e) = query::set_verification_token(&conn, &slug, &user_id, &token, exp) {
            error!("Failed to set verification token: {}", e);

            return;
        }

        let base_url = server_config.base_url();
        let verify_url = format!("{base_url}/admin/verify-email?token={token}");
        let html = match email_renderer.render(
            "verify_email",
            &VerifyEmailContext {
                verify_url: &verify_url,
                from_name: &email_config.from_name,
            },
        ) {
            Ok(h) => h,
            Err(e) => {
                error!("Failed to render verify email template: {}", e);

                return;
            }
        };

        if let Err(e) = queue_email(
            &conn,
            &EmailJobData {
                to: user_email.clone(),
                subject: "Verify your email".to_string(),
                html,
                text: None,
            },
            email_max_attempts,
        ) {
            error!("Failed to queue verification email: {}", e);
        }
    });
}
