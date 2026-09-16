//! Storage backend factory.

use std::{path::Path, sync::Arc};

use anyhow::{Result, bail};

use crate::config::{UploadConfig, UploadStorage};
use crate::core::lua_lease::LuaVmLease;

use super::{CustomStorage, LocalStorage, SharedStorage};

/// Create the appropriate storage backend from config.
///
/// Handles every backend that needs no Lua VM. `storage = "custom"` is
/// refused here: it is delegated to Lua and must be built through
/// [`create_storage_with_lease`].
///
/// # Errors
///
/// Returns an error if the chosen backend fails to initialize, requires a
/// feature the binary wasn't built with, or is `custom` (which needs a lease).
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
        // Falling back to a local backend here would write the operator's
        // files to this machine's disk while `[upload] storage = "custom"`
        // says they belong somewhere else — a silent, unrecoverable
        // misplacement of user data. Every call site that can reach a Lua
        // VM builds the backend through `create_storage_with_lease`.
        UploadStorage::Custom => bail!(
            "`[upload] storage = \"custom\"` needs a Lua VM lease — \
             build the backend with `create_storage_with_lease`."
        ),
    }
}

/// Create a storage backend, backing a `custom` backend with `lease`.
///
/// Use this at call sites that have a Lua VM lease (a hook-runner pool
/// lease, or a per-VM local lease) so `[upload] storage = "custom"`
/// resolves to a working [`CustomStorage`]. It is the only way to build a
/// custom backend — [`create_storage`] refuses one. Non-custom backends
/// ignore the lease.
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

    /// Regression: a lease-less `create_storage` downgraded `custom` to a
    /// local backend, so an operator who configured storage elsewhere had
    /// user files written to this machine's disk instead — silently, and
    /// reported as success.
    #[test]
    fn custom_backend_without_a_lease_is_refused_rather_than_written_locally() {
        let tmp = tempfile::tempdir().unwrap();
        let config = UploadConfig {
            storage: UploadStorage::Custom,
            ..Default::default()
        };

        let err = create_storage(tmp.path(), &config)
            .err()
            .expect("custom storage needs a lease");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("create_storage_with_lease"),
            "the error should name the path that works, got: {msg}"
        );

        assert!(
            !tmp.path().join("uploads").exists(),
            "nothing may be written locally for a custom backend"
        );
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
