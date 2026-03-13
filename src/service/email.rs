//! Fire-and-forget email operations (verification emails).

use std::sync::Arc;

use serde_json::json;

use crate::{
    config::{EmailConfig, ServerConfig},
    core::email::{EmailRenderer, is_configured, send_email},
    db::{DbPool, query},
};

/// Generate a verification token and send the verification email.
/// Spawns its own `spawn_blocking` task internally.
// Excluded from coverage: async tokio task that requires SMTP email transport,
// DB pool, and email renderer — cannot be unit tested without external services.
#[cfg(not(tarpaulin_include))]
pub fn send_verification_email(
    pool: DbPool,
    email_config: EmailConfig,
    email_renderer: Arc<EmailRenderer>,
    server_config: ServerConfig,
    slug: String,
    user_id: String,
    user_email: String,
) {
    tokio::task::spawn_blocking(move || {
        if !is_configured(&email_config) {
            tracing::warn!(
                "Email not configured — skipping verification email for {}",
                user_email
            );

            return;
        }

        let token = nanoid::nanoid!(32);
        let exp = chrono::Utc::now().timestamp() + 86400; // 24 hours

        let conn = match pool.get() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("DB connection for verification token: {}", e);

                return;
            }
        };

        if let Err(e) = query::set_verification_token(&conn, &slug, &user_id, &token, exp) {
            tracing::error!("Failed to set verification token: {}", e);

            return;
        }

        let verify_url = format!(
            "http://{}:{}/admin/verify-email?token={}",
            server_config.host, server_config.admin_port, token
        );
        let data = json!({ "verify_url": verify_url });
        let html = match email_renderer.render("verify_email", &data) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("Failed to render verify email template: {}", e);

                return;
            }
        };

        if let Err(e) = send_email(&email_config, &user_email, "Verify your email", &html, None) {
            tracing::error!("Failed to send verification email: {}", e);
        }
    });
}
