//! Serving an uploaded file from a non-local storage backend (S3, or a
//! custom Lua backend).
//!
//! The local backend hands the file to `tower_http::services::ServeFile`,
//! which answers ranges and conditional GETs itself. A remote object has no
//! filesystem path, so this module reproduces the same contract over the
//! storage trait's ranged read: `206` with a truthful `Content-Range` for a
//! `Range` request, `416` for one the object cannot satisfy, `304` for a
//! matching `If-None-Match` / `If-Modified-Since`, and a strong `ETag` on
//! every answer. The headers themselves come from the one shared path in
//! [`super::headers`], so the two backends cannot drift apart.

use axum::{
    body::Body,
    http::{
        Error as HttpError, StatusCode,
        header::{CONTENT_RANGE, CONTENT_TYPE, ETAG, LAST_MODIFIED},
    },
    response::{IntoResponse, Response},
};
use tokio::task;

use crate::admin::handlers::uploads::headers::{
    ConditionalHeaders, ServeHeaders, is_fresh, parse_range, remote_etag,
};
use crate::core::upload::{ByteRange, RangedObject, SharedStorage, StorageNotFound};

/// Read a key off the async runtime. S3 and custom backends do blocking work
/// — network I/O for S3, a pooled Lua VM call for custom — so the read must
/// run on a blocking thread, never on a tokio worker.
async fn read_blocking(
    storage: &SharedStorage,
    key: String,
    range: Option<ByteRange>,
) -> anyhow::Result<Option<RangedObject>> {
    let storage = storage.clone();

    task::spawn_blocking(move || storage.get_range(&key, range)).await?
}

/// `bytes <first>-<last>/<total>` for the slice actually read, or `None` when
/// the read covers the whole object (a `200`, not a `206`).
fn content_range(object: &RangedObject) -> Option<String> {
    let (first, last) = object.range?;

    let total = object
        .total_size
        .map_or_else(|| "*".to_string(), |total| total.to_string());

    Some(format!("bytes {first}-{last}/{total}"))
}

/// `304`: the viewer's cached copy is current, so the body is omitted and the
/// validators are repeated.
fn not_modified(etag: &str, object: &RangedObject, headers: &ServeHeaders<'_>) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .header(ETAG, etag);

    if let Some(last_modified) = object.last_modified.as_deref() {
        builder = builder.header(LAST_MODIFIED, last_modified);
    }

    finish(builder.body(Body::empty()), headers)
}

/// `416`: the requested range lies outside the object. The object's size is
/// not always known here (a remote can refuse the range without reporting
/// one), so `Content-Range` is emitted only when it would be truthful.
fn unsatisfiable(headers: &ServeHeaders<'_>) -> Response {
    let builder = Response::builder().status(StatusCode::RANGE_NOT_SATISFIABLE);

    finish(builder.body(Body::empty()), headers)
}

/// Apply the shared headers to a built response, falling back to a `500` if
/// the builder rejected a value rather than panicking the request task.
fn finish(built: Result<Response, HttpError>, headers: &ServeHeaders<'_>) -> Response {
    let mut response = built.unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());

    headers.apply(&mut response);
    response
}

/// `200` with the whole object, or `206` with the slice that was read.
fn with_body(etag: &str, object: RangedObject, headers: &ServeHeaders<'_>) -> Response {
    let range = content_range(&object);

    let status = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    let mut builder = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, headers.mime())
        .header(ETAG, etag);

    if let Some(last_modified) = object.last_modified.as_deref() {
        builder = builder.header(LAST_MODIFIED, last_modified);
    }

    if let Some(range) = range.as_deref() {
        builder = builder.header(CONTENT_RANGE, range);
    }

    finish(builder.body(Body::from(object.data)), headers)
}

/// Serve `key` from a remote backend, honouring the request's `Range` and
/// conditional headers.
pub(super) async fn serve_remote(
    storage: &SharedStorage,
    key: &str,
    conditional: &ConditionalHeaders,
    headers: &ServeHeaders<'_>,
) -> Response {
    // A range the object cannot satisfy answers 416 before conditional
    // freshness is evaluated: the validators live on the object, and reading
    // it again unranged purely to answer 304 would cost the round trip the
    // range was meant to avoid.
    let range = parse_range(conditional.range.as_ref());

    let object = match read_blocking(storage, key.to_string(), range).await {
        Ok(Some(object)) => object,
        Ok(None) => return unsatisfiable(headers),
        Err(e) if e.downcast_ref::<StorageNotFound>().is_some() => {
            return StatusCode::NOT_FOUND.into_response();
        }
        // Transient / infrastructure failure (remote network error,
        // VM-pool-acquire timeout under load, …): a retryable 503, not a
        // cacheable 404 for a file that exists.
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let etag = remote_etag(key, &object);

    if is_fresh(conditional, &etag, object.last_modified.as_deref()) {
        return not_modified(&etag, &object, headers);
    }

    with_body(&etag, object, headers)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::Result;
    use axum::http::header::{ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION};
    use http_body_util::BodyExt as _;

    use super::*;
    use crate::core::upload::StorageBackend;

    /// A whole-file backend standing in for a custom Lua backend: it only
    /// knows `get`, so its ranged reads come from the trait's default.
    struct FakeRemote {
        data: Vec<u8>,
        missing: bool,
    }

    impl StorageBackend for FakeRemote {
        fn put(&self, _key: &str, _data: &[u8], _content_type: &str) -> Result<()> {
            Ok(())
        }

        fn get(&self, key: &str) -> Result<Vec<u8>> {
            if self.missing {
                return Err(StorageNotFound(key.to_string()).into());
            }

            Ok(self.data.clone())
        }

        fn delete(&self, _key: &str) -> Result<()> {
            Ok(())
        }

        fn exists(&self, _key: &str) -> Result<bool> {
            Ok(!self.missing)
        }

        fn kind(&self) -> &'static str {
            "fake-remote"
        }
    }

    fn storage(data: &[u8]) -> SharedStorage {
        Arc::new(FakeRemote {
            data: data.to_vec(),
            missing: false,
        })
    }

    fn conditional(range: Option<&str>, if_none_match: Option<&str>) -> ConditionalHeaders {
        ConditionalHeaders {
            range: range.map(|v| v.parse().unwrap()),
            if_none_match: if_none_match.map(|v| v.parse().unwrap()),
            if_modified_since: None,
        }
    }

    fn headers() -> ServeHeaders<'static> {
        ServeHeaders::new("application/pdf", "private, no-store")
            .stored_name(Some("abcdefghij_report.pdf"))
    }

    async fn body_of(response: Response) -> Vec<u8> {
        response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes()
            .to_vec()
    }

    /// Regression: a remote backend buffered and returned the whole object for
    /// every request, ignoring `Range` entirely.
    #[tokio::test]
    async fn a_ranged_request_is_answered_with_the_slice_and_a_content_range() {
        let storage = storage(b"0123456789");
        let cond = conditional(Some("bytes=2-4"), None);

        let response = serve_remote(&storage, "media/a.pdf", &cond, &headers()).await;

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(CONTENT_RANGE).unwrap(),
            "bytes 2-4/10"
        );
        assert_eq!(response.headers().get(ACCEPT_RANGES).unwrap(), "bytes");
        assert_eq!(body_of(response).await, b"234");
    }

    #[tokio::test]
    async fn a_suffix_range_returns_the_tail_of_the_object() {
        let storage = storage(b"0123456789");
        let cond = conditional(Some("bytes=-3"), None);

        let response = serve_remote(&storage, "media/a.pdf", &cond, &headers()).await;

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(CONTENT_RANGE).unwrap(),
            "bytes 7-9/10"
        );
        assert_eq!(body_of(response).await, b"789");
    }

    #[tokio::test]
    async fn an_unsatisfiable_range_is_refused_rather_than_served_whole() {
        let storage = storage(b"0123456789");
        let cond = conditional(Some("bytes=50-60"), None);

        let response = serve_remote(&storage, "media/a.pdf", &cond, &headers()).await;

        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert!(body_of(response).await.is_empty());
    }

    #[tokio::test]
    async fn a_request_without_a_range_is_a_plain_200_with_an_entity_tag() {
        let storage = storage(b"0123456789");
        let cond = conditional(None, None);

        let response = serve_remote(&storage, "media/a.pdf", &cond, &headers()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(CONTENT_RANGE).is_none());
        assert!(response.headers().get(ETAG).is_some());
        assert_eq!(
            response.headers().get(CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"report.pdf\""
        );
        assert_eq!(
            response.headers().get(CACHE_CONTROL).unwrap(),
            "private, no-store"
        );
    }

    /// The tag a remote object is served under must satisfy the next
    /// conditional request: a matching `If-None-Match` answers `304` with no
    /// body at all.
    #[tokio::test]
    async fn a_matching_entity_tag_answers_304_without_a_body() {
        let storage = storage(b"0123456789");

        let first = serve_remote(
            &storage,
            "media/a.pdf",
            &conditional(None, None),
            &headers(),
        )
        .await;
        let etag = first
            .headers()
            .get(ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let cond = conditional(None, Some(&etag));
        let response = serve_remote(&storage, "media/a.pdf", &cond, &headers()).await;

        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers().get(ETAG).unwrap(), &etag);
        assert!(body_of(response).await.is_empty());
    }

    #[tokio::test]
    async fn a_missing_object_is_a_404() {
        let storage: SharedStorage = Arc::new(FakeRemote {
            data: Vec::new(),
            missing: true,
        });

        let response = serve_remote(
            &storage,
            "media/a.pdf",
            &conditional(None, None),
            &headers(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
