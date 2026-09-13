//! Fire-and-forget email operations (verification emails).
//!
//! Both the sign-up email and the self-service resend mint their token
//! through [`issue_verification_token`] and render through the same
//! [`VerificationMailer`], so the two paths cannot drift in link shape,
//! template, or lifetime.

use std::sync::Arc;

use tracing::{error, warn};

use crate::{
    config::{EmailConfig, LocaleConfig, ServerConfig},
    core::{
        CollectionDefinition,
        email::{EmailJobData, EmailRenderer, VerifyEmailContext, is_configured, queue_email},
    },
    db::{DbConnection, DbPool},
    service::{
        ServiceContext,
        auth::{VERIFICATION_TOKEN_EXPIRY, generate_verification_token, issue_verification_token},
    },
};

/// Everything needed to render and queue a verification email. Built once
/// per call site and consumed, so a plain struct literal is enough.
pub(crate) struct VerificationMailer {
    pub email_config: EmailConfig,
    pub email_renderer: Arc<EmailRenderer>,
    pub server_config: ServerConfig,
    pub email_max_attempts: u32,
}

impl VerificationMailer {
    /// Whether an email transport is configured at all.
    ///
    /// `context` identifies the flow, never the address: on the resend path
    /// the address is an unauthenticated request field, and `tracing` emits
    /// embedded newlines raw, so interpolating it would let anyone forge log
    /// lines.
    fn is_configured(&self, context: &str) -> bool {
        if is_configured(&self.email_config) {
            return true;
        }

        warn!("Email not configured — skipping the verification email ({context})");

        false
    }

    /// Render the verification email for `token` and queue it. Every failure
    /// is logged and swallowed — the caller has already returned.
    fn queue(&self, conn: &dyn DbConnection, recipient: &str, token: &str) {
        let base_url = self.server_config.base_url();
        let verify_url = format!("{base_url}/admin/verify-email?token={token}");

        let html = match self.email_renderer.render(
            "verify_email",
            &VerifyEmailContext {
                verify_url: &verify_url,
                from_name: &self.email_config.from_name,
            },
        ) {
            Ok(h) => h,
            Err(e) => {
                error!("Failed to render verify email template: {e}");

                return;
            }
        };

        if let Err(e) = queue_email(
            conn,
            &EmailJobData {
                to: recipient.to_string(),
                subject: "Verify your email".to_string(),
                html,
                text: None,
            },
            self.email_max_attempts,
        ) {
            error!("Failed to queue verification email: {e}");
        }
    }
}

/// Inputs for [`send_verification_email`] — the sign-up path, where the
/// account was just created and its id is already known.
pub(crate) struct VerificationEmailInput {
    pub pool: DbPool,
    pub mailer: VerificationMailer,
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
    tokio::task::spawn_blocking(move || send_verification_email_blocking(&input));
}

/// The blocking body of [`send_verification_email`]. Fire-and-forget — every
/// failure is logged and swallowed here, never surfaced.
#[cfg(not(tarpaulin_include))]
fn send_verification_email_blocking(input: &VerificationEmailInput) {
    if !input.mailer.is_configured("sign-up") {
        return;
    }

    let conn = match input.pool.get() {
        Ok(c) => c,
        Err(e) => {
            error!("DB connection for verification token: {e}");

            return;
        }
    };

    let token = match issue_verification_token(
        &conn,
        &input.slug,
        &input.user_id,
        VERIFICATION_TOKEN_EXPIRY,
    ) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to set verification token: {e}");

            return;
        }
    };

    input.mailer.queue(&conn, &input.user_email, &token);
}

/// Inputs for [`resend_verification_email`] — the self-service path, which
/// only knows the address the caller typed.
pub(crate) struct ResendVerificationInput {
    pub pool: DbPool,
    pub mailer: VerificationMailer,
    pub locale_config: LocaleConfig,
    pub slug: String,
    pub def: Arc<CollectionDefinition>,
    pub email: String,
}

/// Issue a fresh verification token for the account with this address and
/// queue the email.
///
/// Spawns its own `spawn_blocking` task and reports nothing back: the caller
/// answers the same way whether or not an account was found, so the endpoint
/// never confirms which addresses are registered.
#[cfg(not(tarpaulin_include))]
pub(crate) fn resend_verification_email(input: ResendVerificationInput) {
    tokio::task::spawn_blocking(move || resend_verification_email_blocking(&input));
}

#[cfg(not(tarpaulin_include))]
fn resend_verification_email_blocking(input: &ResendVerificationInput) {
    if !input.mailer.is_configured("resend") {
        return;
    }

    let conn = match input.pool.get() {
        Ok(c) => c,
        Err(e) => {
            error!("DB connection for verification resend: {e}");

            return;
        }
    };

    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .conn(&conn)
        .locale_config(Some(&input.locale_config))
        .build();

    let issued = match generate_verification_token(&ctx, &input.email, VERIFICATION_TOKEN_EXPIRY) {
        Ok(Some(r)) => r,
        // No account, already verified, or locked — nothing to send.
        Ok(None) => return,
        Err(e) => {
            error!("Verification resend error: {e}");

            return;
        }
    };

    input.mailer.queue(&conn, &issued.email, &issued.token);
}
