//! Rate limiting with pluggable backends.
//!
//! Provides `LoginRateLimiter` (sliding-window per-key blocking) and
//! `GrpcRateLimiter` (sliding-window per-IP request limiting), both
//! backed by a [`RateLimitBackend`] trait.
//!
//! Backends: `memory` (default), `redis` (feature-flagged), `none` (disabled).
//! The trait + [`SharedRateLimitBackend`] type alias live in the sibling
//! [`backend`] module; sub-modules implement the trait.

mod attempt;
mod backend;
mod factory;
mod grpc;
mod login;
mod memory;
mod none;
#[cfg(feature = "redis")]
mod redis;

pub use attempt::AttemptBudget;
pub use backend::{RateLimitBackend, SharedRateLimitBackend};
pub use factory::{RateLimitFactoryConfig, create_rate_limit_backend};
pub use grpc::GrpcRateLimiter;
pub use login::{
    IP_MCP_API_KEY_KEYSPACE, IP_RESEND_VERIFICATION_KEYSPACE, IP_RESET_PASSWORD_KEYSPACE,
    IP_VERIFY_EMAIL_KEYSPACE, LoginRateLimiter, MFA_ISSUE_KEYSPACE, RESEND_VERIFICATION_KEYSPACE,
};
pub use memory::MemoryRateLimitBackend;
pub use none::NoneRateLimitBackend;
