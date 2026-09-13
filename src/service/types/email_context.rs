//! Bundled email configuration carried through the service layer.

use std::sync::Arc;

use crate::{
    config::{EmailConfig, LocaleConfig, ServerConfig},
    core::{Builder, CollectionDefinition, email::EmailRenderer},
    db::DbPool,
    service::{
        ResendVerificationInput, VerificationEmailInput, VerificationMailer,
        resend_verification_email, send_verification_email,
    },
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

/// Everything the self-service resend needs beyond the mailer itself.
///
/// Built at two call sites (the admin action and the gRPC handler), so it
/// takes the builder the project's >2-field rule asks for.
#[derive(Builder)]
pub(crate) struct ResendTarget {
    #[builder(required)]
    pub pool: DbPool,
    #[builder(required)]
    pub locale_config: LocaleConfig,
    #[builder(required)]
    pub slug: String,
    #[builder(required)]
    pub def: Arc<CollectionDefinition>,
    #[builder(required)]
    pub email: String,
}

impl EmailContext {
    /// The render-and-queue half of the verification flow, detached from
    /// `self` so it can move into a blocking task.
    pub(crate) fn verification_mailer(&self) -> VerificationMailer {
        VerificationMailer {
            email_config: self.email_config.clone(),
            email_renderer: self.email_renderer.clone(),
            server_config: self.server_config.clone(),
            email_max_attempts: self.email_max_attempts,
        }
    }

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
            mailer: self.verification_mailer(),
            slug,
            user_id: doc_id,
            user_email: email,
        });
    }

    /// Spawn a self-service resend. Fire-and-forget, and deliberately silent
    /// about whether the address belongs to an account.
    pub(crate) fn resend_verification(&self, target: ResendTarget) {
        resend_verification_email(ResendVerificationInput {
            pool: target.pool,
            mailer: self.verification_mailer(),
            locale_config: target.locale_config,
            slug: target.slug,
            def: target.def,
            email: target.email,
        });
    }
}
