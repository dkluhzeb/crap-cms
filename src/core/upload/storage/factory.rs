//! Storage backend factory.

use std::{path::Path, sync::Arc};

use anyhow::Result;
#[cfg(not(feature = "s3-storage"))]
use anyhow::bail;
use tracing::info;

use crate::config::{UploadConfig, UploadStorage};

use super::{LocalStorage, SharedStorage};

/// Create the appropriate storage backend from config.
///
/// # Errors
///
/// Returns an error if the chosen backend fails to initialize (or requires a
/// feature the binary wasn't built with).
pub fn create_storage(config_dir: &Path, config: &UploadConfig) -> Result<SharedStorage> {
    match config.storage {
        UploadStorage::Local => {
            let base_dir = config_dir.join("uploads");
            Ok(Arc::new(LocalStorage::new(base_dir)))
        }
        #[cfg(feature = "s3-storage")]
        UploadStorage::S3 => super::s3::create_s3_storage(&config.s3),
        #[cfg(not(feature = "s3-storage"))]
        UploadStorage::S3 => bail!(
            "S3 upload storage requires the `s3-storage` feature. \
             Rebuild with `--features s3-storage`."
        ),
        UploadStorage::Custom => {
            // Custom storage is initialized after Lua init via crap.storage.register().
            // Use local as placeholder — Lua will replace it when init.lua runs.
            info!("Custom storage selected — waiting for Lua init");

            let base_dir = config_dir.join("uploads");

            Ok(Arc::new(LocalStorage::new(base_dir)))
        }
    }
}
