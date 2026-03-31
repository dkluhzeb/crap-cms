//! S3-compatible storage backend (AWS S3, MinIO, Cloudflare R2, etc.).
//!
//! Enabled via `--features s3-storage`.

use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use s3::creds::Credentials;
use s3::{Bucket, Region};
use tokio::task::block_in_place;

use crate::config::S3Config;

use super::{SharedStorage, StorageBackend};

/// S3-compatible storage backend.
pub struct S3Storage {
    bucket: Box<Bucket>,
    prefix: String,
    public_url_base: String,
}

impl S3Storage {
    /// Build the full object key including prefix.
    fn full_key(&self, key: &str) -> String {
        if self.prefix.is_empty() {
            key.to_string()
        } else {
            format!("{}/{}", self.prefix.trim_end_matches('/'), key)
        }
    }
}

impl StorageBackend for S3Storage {
    fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<()> {
        let full_key = self.full_key(key);
        block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.bucket.put_object_with_content_type(
                &full_key,
                data,
                content_type,
            ))
        })
        .with_context(|| format!("S3 put failed: {full_key}"))?;
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let full_key = self.full_key(key);
        let response = block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.bucket.get_object(&full_key))
        })
        .with_context(|| format!("S3 get failed: {full_key}"))?;

        if response.status_code() == 404 {
            bail!("Object not found: {full_key}");
        }

        Ok(response.to_vec())
    }

    fn delete(&self, key: &str) -> Result<()> {
        let full_key = self.full_key(key);
        block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.bucket.delete_object(&full_key))
        })
        .with_context(|| format!("S3 delete failed: {full_key}"))?;
        Ok(())
    }

    fn exists(&self, key: &str) -> Result<bool> {
        let full_key = self.full_key(key);
        let result = block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.bucket.head_object(&full_key))
        });
        match result {
            Ok(_) => Ok(true),
            Err(e) => {
                // S3 returns 404 for non-existent objects — treat as false.
                // Log other errors so connection/auth issues aren't silently hidden.
                let err_str = e.to_string();
                if err_str.contains("404")
                    || err_str.contains("NoSuchKey")
                    || err_str.contains("Not Found")
                {
                    Ok(false)
                } else {
                    tracing::warn!("S3 exists check failed for '{}': {}", full_key, err_str);
                    Ok(false)
                }
            }
        }
    }

    fn public_url(&self, key: &str) -> String {
        let full_key = self.full_key(key);
        if self.public_url_base.is_empty() {
            // Generate S3 URL
            format!("{}/{}", self.bucket.url(), full_key)
        } else {
            // Use configured CDN/custom URL base
            format!(
                "{}/{}",
                self.public_url_base.trim_end_matches('/'),
                full_key
            )
        }
    }

    fn kind(&self) -> &'static str {
        "s3"
    }
}

/// Create an S3 storage backend from config.
pub fn create_s3_storage(config: &S3Config) -> Result<SharedStorage> {
    if config.bucket.is_empty() {
        bail!("upload.s3.bucket is required for S3 storage backend");
    }

    let region = if let Some(ref endpoint) = config.endpoint {
        Region::Custom {
            region: config.region.clone(),
            endpoint: endpoint.clone(),
        }
    } else {
        config.region.parse::<Region>().unwrap_or(Region::UsEast1)
    };

    let credentials = Credentials::new(
        Some(&config.access_key),
        Some(&config.secret_key),
        None,
        None,
        None,
    )
    .context("Failed to create S3 credentials")?;

    let mut bucket =
        Bucket::new(&config.bucket, region, credentials).context("Failed to create S3 bucket")?;

    if config.path_style {
        bucket = bucket.with_path_style();
    }

    tracing::info!(
        "S3 storage: bucket={}, region={}, prefix={}",
        config.bucket,
        config.region,
        if config.prefix.is_empty() {
            "(none)"
        } else {
            &config.prefix
        }
    );

    Ok(Arc::new(S3Storage {
        bucket,
        prefix: config.prefix.clone(),
        public_url_base: config.public_url_base.clone(),
    }))
}
