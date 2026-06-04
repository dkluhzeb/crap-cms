//! Storage backend factory.

use std::{path::Path, sync::Arc};

use anyhow::Result;
#[cfg(not(feature = "s3-storage"))]
use anyhow::bail;
use tracing::info;

use crate::config::{UploadConfig, UploadStorage};
use crate::core::lua_lease::LuaVmLease;

use super::{CustomStorage, LocalStorage, SharedStorage};

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
            // No lease available here (config-only call site). The
            // pool/local-backed custom backend is built via
            // `create_storage_with_lease`; this placeholder only fires if
            // a caller forgot to use that path.
            info!("Custom storage selected without a Lua lease — using local placeholder");

            let base_dir = config_dir.join("uploads");

            Ok(Arc::new(LocalStorage::new(base_dir)))
        }
    }
}

/// Create a storage backend, backing a `custom` backend with `lease`.
///
/// Use this at call sites that have a Lua VM lease (a hook-runner pool
/// lease, or a per-VM local lease) so `[upload] storage = "custom"`
/// resolves to a working [`CustomStorage`] instead of the local
/// placeholder. Non-custom backends ignore the lease.
///
/// # Errors
///
/// Returns an error if the underlying backend fails to initialize.
pub fn create_storage_with_lease(
    config_dir: &Path,
    config: &UploadConfig,
    lease: Arc<dyn LuaVmLease>,
) -> Result<SharedStorage> {
    if matches!(config.storage, UploadStorage::Custom) {
        return Ok(Arc::new(CustomStorage::new(lease)));
    }
    create_storage(config_dir, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_backend_is_created_and_usable() {
        let tmp = tempfile::tempdir().unwrap();
        let config = UploadConfig {
            storage: UploadStorage::Local,
            ..Default::default()
        };
        let storage = create_storage(tmp.path(), &config).unwrap();
        storage.put("k/x.txt", b"hi", "text/plain").unwrap();
        assert!(storage.exists("k/x.txt").unwrap());
    }

    #[test]
    fn custom_backend_falls_back_to_a_working_local_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        let config = UploadConfig {
            storage: UploadStorage::Custom,
            ..Default::default()
        };
        // Until Lua's `crap.storage.register()` runs, Custom is a working
        // local backend rather than a failure.
        let storage = create_storage(tmp.path(), &config).unwrap();
        storage.put("a.txt", b"x", "text/plain").unwrap();
        assert!(storage.exists("a.txt").unwrap());
    }

    #[cfg(not(feature = "s3-storage"))]
    #[test]
    fn s3_without_feature_errors_with_guidance() {
        let tmp = tempfile::tempdir().unwrap();
        let config = UploadConfig {
            storage: UploadStorage::S3,
            ..Default::default()
        };
        let result = create_storage(tmp.path(), &config);
        assert!(result.is_err(), "S3 without the feature should error");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("s3-storage"),
            "error should name the missing feature, got: {msg}"
        );
    }
}
