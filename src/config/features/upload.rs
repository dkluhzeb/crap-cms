//! Global upload + S3 storage configuration.

use serde::{Deserialize, Serialize};

use crate::config::{S3SecretKey, parsing::serde_filesize};

/// Upload storage backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadStorage {
    /// Local filesystem (the default).
    #[default]
    Local,
    /// S3-compatible object storage (requires the `s3-storage` feature).
    S3,
    /// Backend registered from Lua via `crap.storage.register`.
    Custom,
}

/// Global upload settings (per-collection upload config is separate).
#[derive(Debug, Clone, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
#[serde(default, deny_unknown_fields)]
pub struct UploadConfig {
    /// Storage backend: `local` (default), `s3`, or `custom`.
    pub storage: UploadStorage,
    /// Global max file size in bytes. Default: 50MB.
    /// Accepts integer bytes or human-readable string ("50MB", "1GB").
    #[serde(with = "serde_filesize")]
    pub max_file_size: u64,
    /// S3-compatible storage configuration. Only used when `storage = "s3"`.
    #[serde(default)]
    pub s3: S3Config,
}

/// S3-compatible storage configuration.
#[derive(Debug, Clone, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    /// S3 bucket name.
    #[serde(default)]
    pub bucket: String,
    /// AWS region (e.g., `"us-east-1"`). Default: `"us-east-1"`.
    #[serde(default = "default_s3_region")]
    pub region: String,
    /// S3 endpoint URL. Default: AWS. Set for `MinIO`, R2, etc.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Access key ID.
    #[serde(default)]
    pub access_key: String,
    /// Secret access key. Stored in a redacted-on-Debug/Serialize newtype
    /// so it does not leak via tracing, JSON dumps of `CrapConfig`, or
    /// `crap.config.get` from a Lua hook.
    #[serde(default)]
    pub secret_key: S3SecretKey,
    /// Optional key prefix prepended to all storage keys.
    #[serde(default)]
    pub prefix: String,
    /// Use path-style addressing (required for `MinIO` and some providers).
    #[serde(default)]
    pub path_style: bool,
}

fn default_s3_region() -> String {
    "us-east-1".to_string()
}

/// Manual impl so the no-`[upload.s3]`-section path agrees with the
/// serde field defaults. The derived impl gave `region = ""` while a
/// *present* section with no `region` key got `"us-east-1"` — the same
/// config meant two different regions depending on whether the empty
/// section header existed.
impl Default for S3Config {
    fn default() -> Self {
        Self {
            bucket: String::new(),
            region: default_s3_region(),
            endpoint: None,
            access_key: String::new(),
            secret_key: S3SecretKey::default(),
            prefix: String::new(),
            path_style: false,
        }
    }
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            storage: UploadStorage::default(),
            max_file_size: 52_428_800, // 50MB
            s3: S3Config::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the derived `Default` used to give `region = ""` while
    /// parsing an empty `[upload.s3]` section gave `"us-east-1"` — the
    /// absent-section and empty-section paths must agree.
    #[test]
    fn s3_default_region_matches_the_serde_default() {
        let absent = S3Config::default();
        let empty_section: S3Config = toml::from_str("").expect("empty section parses");

        assert_eq!(absent.region, "us-east-1");
        assert_eq!(absent.region, empty_section.region);
    }

    #[test]
    fn upload_config_defaults() {
        let upload = UploadConfig::default();
        assert_eq!(upload.max_file_size, 52_428_800);
    }
}
