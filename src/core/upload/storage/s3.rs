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

/// Classify whether an error string from `head_object` represents
/// "object does not exist" (true) versus any other failure (false).
/// Pulled out so the classification can be unit-tested without a live
/// bucket — the underlying `s3::error::S3Error` does not expose a
/// stable HTTP-status accessor across versions, so we match the
/// `Display` form against the well-known "not found" markers.
fn is_not_found_error(err_str: &str) -> bool {
    err_str.contains("404") || err_str.contains("NoSuchKey") || err_str.contains("Not Found")
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
                let err_str = e.to_string();

                // Only "object not present" maps to false; auth failures
                // (403), transient outages (5xx), and network errors must
                // surface so callers see real failures rather than a
                // misleading "doesn't exist". The previous fallthrough
                // returned `Ok(false)` for any non-404 error, which let
                // upload-then-verify orphan its DB rows on a 503 and
                // reported permission problems as missing files.
                if is_not_found_error(&err_str) {
                    Ok(false)
                } else {
                    Err(anyhow::anyhow!(
                        "S3 exists check failed for '{}': {}",
                        full_key,
                        err_str
                    ))
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
        // `aws_region::Region::from_str` is infallible — unknown strings
        // fall through to `Custom { region: x, endpoint: x }`, where the
        // garbage region is used as the host. Without `upload.s3.endpoint`
        // set, that's almost certainly a typo (`eu-west-1-` →
        // `Custom { endpoint: "eu-west-1-" }` → DNS resolution fails at
        // first request with no startup hint). Reject at boot.
        match config.region.parse::<Region>() {
            Ok(Region::Custom { region, .. }) => {
                bail!(
                    "upload.s3.region '{region}' is not a recognized AWS region. \
                     Use the standard region code (e.g. 'us-east-1', 'eu-west-1'), \
                     or set upload.s3.endpoint for a custom S3-compatible provider."
                );
            }
            Ok(r) => r,
            // FromStr Err is `Utf8Error`; keep a graceful path even though
            // we never expect to hit it for in-memory config strings.
            Err(e) => bail!("upload.s3.region '{}' is invalid: {e}", config.region),
        }
    };

    let credentials = Credentials::new(
        Some(&config.access_key),
        Some(config.secret_key.as_ref()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::S3Config;

    /// Regression: only "object not present" errors should map to
    /// `Ok(false)` from `exists()`. Other variants (auth, transient,
    /// network) must surface as `Err` so callers don't mistake them for
    /// a missing file.
    #[test]
    fn is_not_found_error_recognises_404_markers() {
        // S3 / CloudFront / generic 404 forms.
        assert!(is_not_found_error(
            "Got HTTP 404: <Error><Code>NoSuchKey</Code></Error>"
        ));
        assert!(is_not_found_error("status: 404, message: NoSuchKey"));
        assert!(is_not_found_error("HTTP 404 Not Found"));
        assert!(is_not_found_error("Not Found"));
    }

    /// Regression: auth, transient, and network failures must NOT be
    /// classified as "not found" — that's the bug the round-9 audit
    /// caught (a 403 silently turning into "doesn't exist" lets
    /// upload-then-verify orphan DB rows).
    #[test]
    fn is_not_found_error_rejects_non_404_failures() {
        assert!(!is_not_found_error("Got HTTP 403: AccessDenied"));
        assert!(!is_not_found_error("Got HTTP 500: InternalError"));
        assert!(!is_not_found_error("Got HTTP 503: SlowDown"));
        assert!(!is_not_found_error(
            "error sending request: connection refused"
        ));
        assert!(!is_not_found_error("dns error: failed to lookup address"));
        assert!(!is_not_found_error("SignatureDoesNotMatch"));
        // Empty / unrelated noise also passes through to error.
        assert!(!is_not_found_error(""));
        assert!(!is_not_found_error("OK"));
    }

    fn s3_config_with_region(region: &str) -> S3Config {
        S3Config {
            bucket: "test-bucket".into(),
            region: region.into(),
            access_key: "AKIA...".into(),
            secret_key: "secret".into(),
            endpoint: None,
            prefix: String::new(),
            public_url_base: String::new(),
            path_style: false,
        }
    }

    /// Regression: a bad `upload.s3.region` (typo, garbage) used to
    /// silently fall back to `us-east-1` via `unwrap_or`, producing
    /// 301 redirects / signature-mismatch errors at first use with
    /// no startup hint. Must now bail with a clear diagnostic.
    #[test]
    fn create_s3_storage_rejects_unparseable_region() {
        let cfg = s3_config_with_region("eu-west-1-typo");
        let result = create_s3_storage(&cfg);
        let err = match result {
            Ok(_) => panic!("expected error for unparseable region"),
            Err(e) => e,
        };
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("eu-west-1-typo") || msg.contains("not a recognized"),
            "expected region diagnostic, got: {msg}"
        );
    }

    /// Sanity: a real region still works (this exercises the success
    /// path of the new `with_context`).
    #[test]
    fn create_s3_storage_accepts_known_region() {
        let cfg = s3_config_with_region("eu-west-1");
        assert!(
            create_s3_storage(&cfg).is_ok(),
            "eu-west-1 must be accepted",
        );
    }

    /// Sanity: a custom endpoint bypasses region parsing entirely
    /// (Custom region carries the user's region string verbatim, so
    /// even non-AWS region names work for S3-compatible providers).
    #[test]
    fn create_s3_storage_with_endpoint_accepts_any_region_string() {
        let mut cfg = s3_config_with_region("auto");
        cfg.endpoint = Some("https://s3.example.com".into());
        assert!(
            create_s3_storage(&cfg).is_ok(),
            "custom endpoint should bypass region parsing",
        );
    }
}
