//! Storage backend abstraction for upload files.
//!
//! Provides a trait-based backend system: `local` (default filesystem),
//! `s3` (S3-compatible, feature-flagged), and `custom` (Lua-delegated).
//!
//! The [`StorageBackend`] trait + [`SharedStorage`] type alias live in the
//! sibling [`backend`] module; sub-modules (`local`, `s3`, `custom`)
//! implement the trait. The factory ([`create_storage`]) picks the
//! implementation by config.

mod backend;
mod custom;
mod factory;
mod local;
#[cfg(feature = "s3-storage")]
mod s3;

pub use crate::config::UploadConfig;
pub use backend::{SharedStorage, StorageBackend};
pub use custom::CustomStorage;
pub use factory::create_storage;
pub use local::LocalStorage;
