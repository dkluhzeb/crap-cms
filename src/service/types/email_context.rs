//! Bundled email configuration carried through the service layer.

use std::sync::Arc;

use crate::{
    config::{EmailConfig, ServerConfig},
    core::email::EmailRenderer,
    db::DbPool,
};

/// Bundled email configuration for verification emails.
/// Cloning is cheap (configs are small, renderer is Arc).
#[derive(Clone)]
pub struct EmailContext {
    pub email_config: EmailConfig,
    pub email_renderer: Arc<EmailRenderer>,
    pub server_config: ServerConfig,
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
        crate::service::send_verification_email(
            pool,
            self.email_config.clone(),
            self.email_renderer.clone(),
            self.server_config.clone(),
            slug,
            doc_id,
            email,
        );
    }
}
