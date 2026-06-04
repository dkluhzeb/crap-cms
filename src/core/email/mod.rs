//! Email sending abstraction and template rendering.
//!
//! The [`EmailProvider`] trait + [`SharedEmailProvider`] type alias live
//! in the sibling [`backend`] module; sub-modules (`smtp`, `webhook`,
//! `log`, `custom`) implement the trait. The factory
//! ([`create_email_provider`]) picks the implementation by config.

mod backend;
mod contexts;
mod custom;
mod factory;
mod log;
pub mod queue;
mod renderer;
mod smtp;
mod validation;
mod webhook;

pub use backend::{EmailProvider, SharedEmailProvider};
pub use contexts::{MfaCodeEmailContext, PasswordResetEmailContext, VerifyEmailContext};
pub use custom::CustomEmailProvider;
pub use factory::{create_email_provider, create_email_provider_with_lease, is_configured};
pub use queue::{EmailJobData, SYSTEM_EMAIL_JOB, SYSTEM_EMAIL_QUEUE, queue_email};
pub use renderer::EmailRenderer;
pub use validation::validate_no_crlf;
