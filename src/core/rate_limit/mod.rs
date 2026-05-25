//! Rate limiting with pluggable backends.
//!
//! Provides `LoginRateLimiter` (sliding-window per-key blocking) and
//! `GrpcRateLimiter` (sliding-window per-IP request limiting), both
//! backed by a [`RateLimitBackend`] trait.
//!
//! Backends: `memory` (default), `redis` (feature-flagged), `none` (disabled).
//! The trait + [`SharedRateLimitBackend`] type alias live in the sibling
//! [`backend`] module; sub-modules implement the trait.

mod backend;
mod factory;
mod grpc;
mod login;
mod memory;
mod none;
#[cfg(feature = "redis")]
mod redis;

pub use backend::{RateLimitBackend, SharedRateLimitBackend};
pub use factory::{RateLimitFactoryConfig, create_rate_limit_backend};
pub use grpc::GrpcRateLimiter;
pub use login::LoginRateLimiter;
pub use memory::MemoryRateLimitBackend;
pub use none::NoneRateLimitBackend;
