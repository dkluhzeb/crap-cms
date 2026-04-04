//! Storage backend factory.

use std::{path::Path, sync::Arc};

use anyhow::{Result, bail};
use tracing::info;

use crate::config::UploadConfig;

use super::{LocalStorage, SharedStorage};

/// Create the appropriate storage backend from config.
pub fn create_storage(config_dir: &Path, config: &UploadConfig) -> Result<SharedStorage> {
    match config.storage.as_str() {
        "local" | "" => {
            let base_dir = config_dir.join("uploads");
            Ok(Arc::new(LocalStorage::new(base_dir)))
        }
        #[cfg(feature = "s3-storage")]
        "s3" => super::s3::create_s3_storage(&config.s3),
        "custom" => {
            // Custom storage is initialized after Lua init via crap.storage.register().
            // Use local as placeholder — Lua will replace it when init.lua runs.
            info!("Custom storage selected — waiting for Lua init");

            let base_dir = config_dir.join("uploads");

            Ok(Arc::new(LocalStorage::new(base_dir)))
        }
        other => bail!("Unknown upload storage backend: '{}'", other),
    }
}
