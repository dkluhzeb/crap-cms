//! S3-compatible storage backend (AWS S3, `MinIO`, Cloudflare R2, etc.).
//!
//! Enabled via `--features s3-storage`.

use std::{future::Future, sync::Arc};

use anyhow::{Context as _, Result, bail};
use s3::creds::Credentials;
use s3::{Bucket, Region};
use tokio::{runtime::Handle, task::block_in_place};

use crate::config::S3Config;

use super::backend::validate_key;
use super::{SharedStorage, StorageBackend, StorageNotFound};

/// S3-compatible storage backend.
pub struct S3Storage {
    bucket: Box<Bucket>,
    prefix: String,
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

/// Drive one bucket request to completion from the synchronous
/// [`StorageBackend`] trait. The trait is blocking while the bucket client is
/// async, so every verb crosses that boundary through this one place.
fn block_on_s3<F: Future>(fut: F) -> F::Output {
    block_in_place(|| Handle::current().block_on(fut))
}

/// What a `404` means for the verb whose response is being classified.
#[derive(Clone, Copy)]
enum Missing {
    /// The object genuinely is not there: the typed [`StorageNotFound`] a
    /// serve handler turns into a 404 (as opposed to a 503 for a transient
    /// failure).
    NotFound,
    /// The verb's goal is already met — deleting an object that is not
    /// there succeeds, matching `LocalStorage::delete`.
    Success,
    /// The request addressed no object, so a `404` is the *bucket* missing:
    /// a real failure, not an absent key.
    Failure,
}

/// Turn an S3 response status into the storage contract's outcome.
///
/// The bucket client is built without `fail-on-err`, so every response —
/// `403`, `500`, `503` included — arrives as an `Ok` carrying the status.
/// Unchecked, a rejected write reports success and leaves a document row
/// pointing at an object that was never stored, and a rejected read hands
/// the provider's error XML back as the file's bytes. The error names the
/// operation, key and status only: the response body can carry request
/// identifiers and bucket internals, so it is never quoted.
fn check_status(op: &str, key: &str, status: u16, missing: Missing) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }

    if status != 404 {
        bail!("S3 {op} failed for '{key}': HTTP {status}");
    }

    match missing {
        Missing::NotFound => Err(StorageNotFound(key.to_string()).into()),
        Missing::Success => Ok(()),
        Missing::Failure => bail!("S3 {op} failed for '{key}': bucket not found (HTTP 404)"),
    }
}

impl StorageBackend for S3Storage {
    fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<()> {
        validate_key(key)?;
        let full_key = self.full_key(key);

        let response = block_on_s3(self.bucket.put_object_with_content_type(
            &full_key,
            data,
            content_type,
        ))
        .with_context(|| format!("S3 put failed: {full_key}"))?;

        check_status("put", &full_key, response.status_code(), Missing::Failure)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>> {
        validate_key(key)?;
        let full_key = self.full_key(key);

        let response = block_on_s3(self.bucket.get_object(&full_key))
            .with_context(|| format!("S3 get failed: {full_key}"))?;

        check_status("get", &full_key, response.status_code(), Missing::NotFound)?;

        Ok(response.to_vec())
    }

    fn delete(&self, key: &str) -> Result<()> {
        validate_key(key)?;
        let full_key = self.full_key(key);

        let response = block_on_s3(self.bucket.delete_object(&full_key))
            .with_context(|| format!("S3 delete failed: {full_key}"))?;

        check_status(
            "delete",
            &full_key,
            response.status_code(),
            Missing::Success,
        )
    }

    fn exists(&self, key: &str) -> Result<bool> {
        // An invalid key definitionally cannot map to a stored object — return
        // false rather than erroring, matching `LocalStorage::exists`.
        if validate_key(key).is_err() {
            return Ok(false);
        }

        let full_key = self.full_key(key);

        // `head_object` carries its HTTP status in the `Ok` tuple; the `Err`
        // arm is transport/signing failures only, which must surface rather
        // than read as "doesn't exist".
        let (_, status) = block_on_s3(self.bucket.head_object(&full_key))
            .with_context(|| format!("S3 exists check failed: {full_key}"))?;

        // Only a confirmed absence is `false`. Auth failures (403) and
        // transient outages (5xx) stay errors so upload-then-verify does not
        // orphan its DB rows and a permission problem is not reported as a
        // missing file.
        match check_status("exists", &full_key, status, Missing::NotFound) {
            Ok(()) => Ok(true),
            Err(e) if e.downcast_ref::<StorageNotFound>().is_some() => Ok(false),
            Err(e) => Err(e),
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
    }))
}

#[cfg(test)]
mod tests {
    use anyhow::Error;

    use super::*;
    use crate::config::S3Config;

    /// True when `err` is the typed "object genuinely absent" marker.
    fn is_not_found(err: &Error) -> bool {
        err.downcast_ref::<StorageNotFound>().is_some()
    }

    /// Regression: `put` ignored the response status entirely, so a bucket
    /// that answered `403`/`500`/`503` returned `Ok(())`. The upload's
    /// cleanup guard then committed a document row pointing at an object
    /// that was never stored.
    #[test]
    fn a_rejected_put_is_an_error() {
        for status in [403, 500, 503] {
            let err = check_status("put", "media/a.png", status, Missing::Failure)
                .expect_err("a non-2xx put must fail");
            assert!(!is_not_found(&err), "a rejected put is not a missing key");
        }

        // A 404 answering a PUT is the bucket missing, not an absent object.
        let err = check_status("put", "media/a.png", 404, Missing::Failure).unwrap_err();
        assert!(!is_not_found(&err), "{err:#}");

        for status in [200, 201, 204] {
            assert!(check_status("put", "media/a.png", status, Missing::Failure).is_ok());
        }
    }

    /// Regression: `get` mapped only `404`, so a `403`/`5xx` handed the
    /// provider's error XML back as the file's bytes — which the serve
    /// route shipped as a `200` under the stored image content type (and,
    /// on a public collection, an immutable cache header).
    #[test]
    fn a_rejected_get_is_an_error_not_file_bytes() {
        for status in [403, 500, 503] {
            let err = check_status("get", "media/a.png", status, Missing::NotFound)
                .expect_err("a non-2xx get must fail");
            assert!(
                !is_not_found(&err),
                "a transient/auth failure must not read as a missing key: {err:#}"
            );
        }

        let missing = check_status("get", "media/a.png", 404, Missing::NotFound).unwrap_err();
        assert!(is_not_found(&missing), "{missing:#}");

        assert!(check_status("get", "media/a.png", 200, Missing::NotFound).is_ok());
    }

    /// Regression: `delete` ignored the response status, so a rejected
    /// delete reported success and left the object orphaned in the bucket.
    /// A `404` stays a success — deleting something that is not there is
    /// the same no-op `LocalStorage::delete` performs.
    #[test]
    fn a_rejected_delete_is_an_error_but_a_missing_object_is_not() {
        for status in [403, 500, 503] {
            assert!(
                check_status("delete", "media/a.png", status, Missing::Success).is_err(),
                "a non-2xx delete must fail (status {status})"
            );
        }

        assert!(check_status("delete", "media/a.png", 204, Missing::Success).is_ok());
        assert!(
            check_status("delete", "media/a.png", 404, Missing::Success).is_ok(),
            "deleting an absent object matches LocalStorage: success"
        );
    }

    /// Regression: `exists` looked for a `404` in the `Err` arm, but
    /// `head_object` carries its status in the `Ok` tuple — so a missing
    /// object reported `true`. The classification `exists` maps to
    /// `Ok(false)` is the typed not-found, and nothing else.
    #[test]
    fn exists_classifies_only_a_404_as_absent() {
        assert!(check_status("exists", "media/a.png", 200, Missing::NotFound).is_ok());

        let missing = check_status("exists", "media/a.png", 404, Missing::NotFound).unwrap_err();
        assert!(is_not_found(&missing), "a 404 head is an absent object");

        for status in [403, 500, 503] {
            let err = check_status("exists", "media/a.png", status, Missing::NotFound).unwrap_err();
            assert!(
                !is_not_found(&err),
                "an auth/transient failure must surface, not read as absent: {err:#}"
            );
        }
    }

    /// The failure message identifies the operation, key and status — and
    /// never quotes the response body, which can carry request identifiers
    /// and bucket internals.
    #[test]
    fn a_failure_names_the_operation_key_and_status() {
        let err = check_status("put", "media/a.png", 503, Missing::Failure).unwrap_err();
        let msg = format!("{err:#}");

        assert!(msg.contains("put"), "{msg}");
        assert!(msg.contains("media/a.png"), "{msg}");
        assert!(msg.contains("503"), "{msg}");
    }

    /// Regression: the storage key contract (`validate_key`) is enforced on
    /// S3 exactly as on `LocalStorage` — a traversal / absolute / null-byte
    /// key is rejected at the boundary, *before* any network request, so no
    /// live bucket or runtime is needed to prove it. Prevents malformed keys
    /// from reaching the bucket and keeps all three backends in parity.
    #[test]
    fn rejects_malformed_keys_before_any_network_call() {
        let storage = create_s3_storage(&s3_config_with_region("eu-west-1")).unwrap();

        assert!(storage.put("../escape.txt", b"x", "text/plain").is_err());
        assert!(storage.get("../escape.txt").is_err());
        assert!(storage.delete("../escape.txt").is_err());
        assert!(storage.put("/etc/passwd", b"x", "text/plain").is_err());
        assert!(storage.put("ok\0hidden", b"x", "text/plain").is_err());

        // exists() maps an invalid key to "not present", matching LocalStorage.
        assert!(!storage.exists("../escape.txt").unwrap());
    }

    fn s3_config_with_region(region: &str) -> S3Config {
        S3Config {
            bucket: "test-bucket".into(),
            region: region.into(),
            access_key: "AKIA...".into(),
            secret_key: "secret".into(),
            endpoint: None,
            prefix: String::new(),
            path_style: false,
        }
    }

    /// Regression: a bad `upload.s3.region` (typo, garbage) used to
    /// silently fall back to `us-east-1` via `unwrap_or`, producing
    /// 301 redirects / signature-mismatch errors at first use with
    /// no startup hint. Must now bail with a clear diagnostic.
    #[test]
    fn create_s3_storage_rejects_unparsable_region() {
        let cfg = s3_config_with_region("eu-west-1-typo");
        let result = create_s3_storage(&cfg);
        let Err(err) = result else {
            panic!("expected error for unparsable region");
        };
        let msg = format!("{err:#}");
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
