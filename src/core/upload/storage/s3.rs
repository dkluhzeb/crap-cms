//! S3-compatible storage backend (AWS S3, `MinIO`, Cloudflare R2, etc.).
//!
//! Enabled via `--features s3-storage`.

use std::{collections::HashMap, future::Future, sync::Arc};

use anyhow::{Context as _, Result, bail};
use s3::creds::Credentials;
use s3::{Bucket, Region, request::ResponseData};
use tokio::{runtime::Handle, task::block_in_place};

use crate::config::S3Config;

use super::backend::validate_key;
use super::{ByteRange, RangedObject, SharedStorage, StorageBackend, StorageNotFound};

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

/// The bounds of one ranged read, in the form the bucket client puts on the
/// wire: `Range: bytes=<start>-<end>`, open-ended when `end` is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RangeBounds {
    start: u64,
    end: Option<u64>,
}

impl RangeBounds {
    /// The exact `Range` header value the request carries.
    fn header_value(self) -> String {
        match self.end {
            Some(end) => format!("bytes={}-{end}", self.start),
            None => format!("bytes={}-", self.start),
        }
    }
}

/// Look a response header up case-insensitively — header names reach us as
/// whatever casing the provider sent.
fn header<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// An S3 entity tag with its transport quoting removed, so it can be re-quoted
/// once by whoever emits an HTTP `ETag`.
fn unquoted_etag(headers: &HashMap<String, String>) -> Option<String> {
    let raw = header(headers, "etag")?.trim();
    let value = raw.trim_start_matches('"').trim_end_matches('"');

    (!value.is_empty()).then(|| value.to_string())
}

/// Parse `Content-Range: bytes <first>-<last>/<total>` into the inclusive
/// offsets served and the object's total size (`None` for an unknown `*`).
fn parse_content_range(value: &str) -> Option<((u64, u64), Option<u64>)> {
    let spec = value.trim().strip_prefix("bytes ")?.trim();
    let (offsets, total) = spec.split_once('/')?;
    let (first, last) = offsets.split_once('-')?;

    let first: u64 = first.trim().parse().ok()?;
    let last: u64 = last.trim().parse().ok()?;

    if first > last {
        return None;
    }

    Some(((first, last), total.trim().parse().ok()))
}

/// Turn a ranged bucket response into the storage contract's read result.
///
/// A provider that ignored the `Range` header and answered `200` is reported
/// as a whole-object read, so the caller slices locally instead of serving the
/// whole object under a `Content-Range` that claims a slice.
fn ranged_object(response: &ResponseData, bounds: RangeBounds) -> Option<RangedObject> {
    let headers = response.headers();
    let data = response.as_slice().to_vec();
    let len = u64::try_from(data.len()).unwrap_or(0);
    let etag = unquoted_etag(&headers);
    let last_modified = header(&headers, "last-modified").map(str::to_string);
    let described = |bytes: Vec<u8>| {
        RangedObject::whole(bytes)
            .etag(etag.clone())
            .last_modified(last_modified.clone())
    };

    if let Some((offsets, total)) = header(&headers, "content-range").and_then(parse_content_range)
    {
        return Some(
            described(data)
                .range(Some(offsets))
                .total_size(total)
                .build(),
        );
    }

    // A provider that answered `206` without a `Content-Range` still sent the
    // slice that was asked for; place it from the bounds the request carried.
    if response.status_code() == 206 && len > 0 {
        let placed = (bounds.start, bounds.start + len - 1);

        return Some(described(data).range(Some(placed)).build());
    }

    // The provider ignored the `Range` header and answered with the whole
    // object: cut the requested window out here, so a ranged request never
    // serves more than it asked for. `None` when the window lies outside the
    // object, which the serve route answers with `416`.
    let last_byte = len.checked_sub(1)?;
    let last = bounds.end.map_or(last_byte, |end| end.min(last_byte));

    if bounds.start > last {
        return None;
    }

    let from = usize::try_from(bounds.start).ok()?;
    let to = usize::try_from(last).ok()?;
    let slice = data.get(from..=to)?.to_vec();

    Some(
        described(slice)
            .range(Some((bounds.start, last)))
            .total_size(Some(len))
            .build(),
    )
}

impl S3Storage {
    /// Read a whole object, carrying its validators (`ETag`, `Last-Modified`)
    /// back so a conditional request can be answered against them. `full_key`
    /// is already prefixed and validated.
    fn get_whole(&self, full_key: &str) -> Result<RangedObject> {
        let response = block_on_s3(self.bucket.get_object(full_key))
            .with_context(|| format!("S3 get failed: {full_key}"))?;

        check_status("get", full_key, response.status_code(), Missing::NotFound)?;

        let headers = response.headers();

        Ok(RangedObject::whole(response.to_vec())
            .etag(unquoted_etag(&headers))
            .last_modified(header(&headers, "last-modified").map(str::to_string))
            .build())
    }

    /// Turn a requested range into wire bounds. A suffix range needs the
    /// object's size, which only a `HEAD` can tell us — that costs one extra
    /// round trip on the rare suffix request, and still never transfers the
    /// object. `Ok(None)` means the range cannot be satisfied.
    fn bounds_for(&self, full_key: &str, range: ByteRange) -> Result<Option<RangeBounds>> {
        let n = match range {
            // An inverted range (`bytes=5-2`) is unsatisfiable, and would trip
            // the bucket client's own `start <= end` assertion.
            ByteRange::Offset { start, end } if end.is_some_and(|end| start > end) => {
                return Ok(None);
            }
            ByteRange::Offset { start, end } => return Ok(Some(RangeBounds { start, end })),
            ByteRange::Suffix(n) => n,
        };

        let (head, status) = block_on_s3(self.bucket.head_object(full_key))
            .with_context(|| format!("S3 head failed: {full_key}"))?;

        check_status("head", full_key, status, Missing::NotFound)?;

        let Some(total) = head
            .content_length
            .and_then(|bytes| u64::try_from(bytes).ok())
        else {
            // Without a size we cannot place a suffix range; fall back to the
            // whole object rather than guess an offset.
            return Ok(Some(RangeBounds {
                start: 0,
                end: None,
            }));
        };

        let Some((first, last)) = ByteRange::Suffix(n).resolve(total) else {
            return Ok(None);
        };

        Ok(Some(RangeBounds {
            start: first,
            end: Some(last),
        }))
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

        Ok(self.get_whole(&self.full_key(key))?.data)
    }

    fn get_range(&self, key: &str, range: Option<ByteRange>) -> Result<Option<RangedObject>> {
        validate_key(key)?;
        let full_key = self.full_key(key);

        let Some(range) = range else {
            return self.get_whole(&full_key).map(Some);
        };

        let Some(bounds) = self.bounds_for(&full_key, range)? else {
            return Ok(None);
        };

        let response = block_on_s3(self.bucket.get_object_range(
            &full_key,
            bounds.start,
            bounds.end,
        ))
        .with_context(|| {
            format!(
                "S3 ranged get failed: {full_key} ({})",
                bounds.header_value()
            )
        })?;

        // The provider answers 416 for a range that starts past the end of the
        // object; that is the caller's `Ok(None)`, not a failure.
        if response.status_code() == 416 {
            return Ok(None);
        }

        check_status("get", &full_key, response.status_code(), Missing::NotFound)?;

        Ok(ranged_object(&response, bounds))
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

    /// A backend pointed at a custom endpoint: enough to exercise everything
    /// that happens before a request leaves the process.
    fn offline_storage() -> S3Storage {
        let region = Region::Custom {
            region: "eu-west-1".into(),
            endpoint: "https://s3.example.invalid".into(),
        };
        let credentials =
            Credentials::new(Some("AKIA..."), Some("secret"), None, None, None).unwrap();

        S3Storage {
            bucket: Bucket::new("test-bucket", region, credentials).unwrap(),
            prefix: String::new(),
        }
    }

    /// Regression: a remote read used to pull the whole object for every
    /// request. A ranged read must put the requested slice on the wire as an
    /// HTTP `Range` header — asserted on the bounds the request is built
    /// from, so no live bucket is needed.
    #[test]
    fn a_ranged_read_asks_the_bucket_for_only_the_requested_slice() {
        let storage = offline_storage();

        let closed = storage
            .bounds_for("media/a.bin", ByteRange::inclusive(10, 19))
            .unwrap()
            .expect("a closed range is satisfiable");
        assert_eq!(closed.header_value(), "bytes=10-19");

        let open = storage
            .bounds_for("media/a.bin", ByteRange::from_start(64))
            .unwrap()
            .expect("an open range is satisfiable");
        assert_eq!(open.header_value(), "bytes=64-");
    }

    /// An inverted range is unsatisfiable — and must be caught before the
    /// bucket client's own `start <= end` assertion panics the request task.
    #[test]
    fn an_inverted_range_is_unsatisfiable_and_never_reaches_the_client() {
        let storage = offline_storage();

        assert!(
            storage
                .bounds_for("media/a.bin", ByteRange::inclusive(5, 2))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn content_range_parsing_yields_the_offsets_and_the_total() {
        assert_eq!(
            parse_content_range("bytes 10-19/100"),
            Some(((10, 19), Some(100)))
        );
        assert_eq!(parse_content_range("bytes 0-9/*"), Some(((0, 9), None)));

        assert!(parse_content_range("items 0-9/100").is_none());
        assert!(parse_content_range("bytes 19-10/100").is_none());
        assert!(parse_content_range("garbage").is_none());
    }

    #[test]
    fn the_entity_tag_is_reported_without_its_transport_quoting() {
        let mut headers = HashMap::new();
        headers.insert("ETag".to_string(), "\"abc123\"".to_string());

        assert_eq!(unquoted_etag(&headers), Some("abc123".to_string()));
        assert_eq!(header(&headers, "etag"), Some("\"abc123\""));

        headers.insert("ETag".to_string(), "\"\"".to_string());
        assert!(unquoted_etag(&headers).is_none());
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
