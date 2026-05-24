//! Bundled email configuration carried through the service layer.

use std::sync::Arc;

use crate::{
    config::{EmailConfig, ServerConfig},
    core::email::EmailRenderer,
    db::DbPool,
    service::{VerificationEmailInput, send_verification_email},
};

/// Bundled email configuration for verification emails.
/// Cloning is cheap (configs are small, renderer is Arc).
#[derive(Clone)]
pub struct EmailContext {
    pub email_config: EmailConfig,
    pub email_renderer: Arc<EmailRenderer>,
    pub server_config: ServerConfig,
    /// Total attempts (initial + retries) for `_system_email` jobs,
    /// resolved from `[jobs.queues.email] retries` via
    /// `JobsConfig::system_email_max_attempts`.
    pub email_max_attempts: u32,
}

impl EmailContext {
    /// Spawn a verification email send. Fire-and-forget — clones internal
    /// configs (cheap) so the caller doesn't have to.
    pub(crate) fn send_verification(
        &self,
        pool: DbPool,
        slug: String,
        doc_id: String,
        email: String,
    ) {
        send_verification_email(VerificationEmailInput {
            pool,
            email_config: self.email_config.clone(),
            email_renderer: self.email_renderer.clone(),
            server_config: self.server_config.clone(),
            email_max_attempts: self.email_max_attempts,
            slug,
            user_id: doc_id,
            user_email: email,
        });
    }
}
