//! Serving an uploaded file from a non-local storage backend (S3, or a
//! custom Lua backend).
//!
//! The local backend hands the file to `tower_http::services::ServeFile`,
//! which answers ranges and conditional GETs itself. A remote object has no
//! filesystem path, so this module reproduces the same contract over the
//! storage trait: `206` with a truthful `Content-Range` for a `Range` request
//! (honouring `If-Range`), `416` for one the object cannot satisfy, `412` for
//! a failed `If-Match` / `If-Unmodified-Since`, `304` for a matching
//! `If-None-Match` / `If-Modified-Since`, and a strong `ETag` on every answer. The headers themselves come from the one shared path in
//! [`super::headers`], so the two backends cannot drift apart.
//!
//! Two read strategies, chosen by what the backend can do:
//!
//! - **Streamed** — a backend that reports metadata
//!   ([`StorageBackend::stat`](crate::core::upload::StorageBackend::stat): S3
//!   answers a `HEAD`, a custom Lua backend its `stat` handler) is answered
//!   from that metadata first: a failed precondition is a `412` and a
//!   conditional hit a `304` without transferring a byte, an unsatisfiable
//!   range a `416`. The body is then
//!   streamed as a series of ranged reads of at most [`STREAM_CHUNK_BYTES`],
//!   each fetched only when the connection asks for the next chunk — a `HEAD`
//!   or a client that goes away reads nothing more — so memory per request
//!   is bounded whatever the object's size. An object replaced mid-stream
//!   (its entity tag changes) aborts the transfer instead of splicing two
//!   versions.
//! - **Buffered** — a backend that cannot describe an object without reading
//!   it (a custom Lua backend without `stat` / `get_range` handlers hands over
//!   whole strings) is read once, whole, and answered from those bytes.

use std::{io, iter, sync::Arc};

use axum::{
    body::{Body, Bytes},
    http::{
        Error as HttpError, StatusCode,
        header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, LAST_MODIFIED},
        response::Builder,
    },
    response::{IntoResponse, Response},
};
use tokio::task;
use tokio_stream::{StreamExt as _, iter as stream_iter};

use crate::admin::handlers::uploads::{
    headers::{
        ConditionalHeaders, ServeHeaders, if_range_allows, is_fresh, parse_range, remote_etag,
    },
    preconditions::{precondition_failed, preconditions_pass},
};
use crate::core::upload::{
    ByteRange, ObjectMeta, RangedObject, SharedStorage, StorageNotFound, slice_locally,
};

/// Largest slice read from the backend in one call while streaming a body.
#[cfg(not(test))]
const STREAM_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// Tiny in tests, so a ten-byte object exercises the chunking.
#[cfg(test)]
const STREAM_CHUNK_BYTES: u64 = 4;

/// One serve request against a remote backend.
struct RemoteRequest<'a> {
    storage: &'a SharedStorage,
    key: &'a str,
    conditional: &'a ConditionalHeaders,
    headers: &'a ServeHeaders<'a>,
}

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

/// Read a key's metadata off the async runtime (see [`read_blocking`]).
async fn stat_blocking(storage: &SharedStorage, key: String) -> anyhow::Result<Option<ObjectMeta>> {
    let storage = storage.clone();

    task::spawn_blocking(move || storage.stat(&key)).await?
}

/// A failed backend call: `404` for a confirmed miss, a retryable `503` for a
/// transient / infrastructure failure (remote network error, VM-pool-acquire
/// timeout under load, …) — never a cacheable 404 for a file that exists.
fn failure(error: &anyhow::Error) -> Response {
    if error.downcast_ref::<StorageNotFound>().is_some() {
        return StatusCode::NOT_FOUND.into_response();
    }

    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

/// A response builder carrying the object's validators.
fn with_validators(status: StatusCode, etag: &str, last_modified: Option<&str>) -> Builder {
    let builder = Response::builder().status(status).header(ETAG, etag);

    let Some(last_modified) = last_modified else {
        return builder;
    };

    builder.header(LAST_MODIFIED, last_modified)
}

/// `304`: the viewer's cached copy is current, so the body is omitted and the
/// validators are repeated.
fn not_modified(etag: &str, last_modified: Option<&str>, headers: &ServeHeaders<'_>) -> Response {
    let builder = with_validators(StatusCode::NOT_MODIFIED, etag, last_modified);

    finish(builder.body(Body::empty()), headers)
}

/// `416`: the requested range lies outside the object. `Content-Range:
/// bytes */<size>` is emitted only when the size is known — a remote can
/// refuse the range without reporting one.
fn unsatisfiable(size: Option<u64>, headers: &ServeHeaders<'_>) -> Response {
    let mut builder = Response::builder().status(StatusCode::RANGE_NOT_SATISFIABLE);

    if let Some(size) = size {
        builder = builder.header(CONTENT_RANGE, format!("bytes */{size}"));
    }

    finish(builder.body(Body::empty()), headers)
}

/// Apply the shared headers to a built response, falling back to a `500` if
/// the builder rejected a value rather than panicking the request task.
fn finish(built: Result<Response, HttpError>, headers: &ServeHeaders<'_>) -> Response {
    let mut response = built.unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());

    headers.apply(&mut response);
    response
}

/// `bytes <first>-<last>/<total>` (`*` for an unknown total).
fn content_range((first, last): (u64, u64), total: Option<u64>) -> String {
    let total = total.map_or_else(|| "*".to_string(), |total| total.to_string());

    format!("bytes {first}-{last}/{total}")
}

/// What a streamed body covers: the inclusive offsets to send, and whether
/// that is a `206` slice or the whole object.
struct Span {
    offsets: Option<(u64, u64)>,
    partial: bool,
}

impl Span {
    fn new(offsets: Option<(u64, u64)>, partial: bool) -> Self {
        Self { offsets, partial }
    }

    fn len(&self) -> u64 {
        self.offsets.map_or(0, |(first, last)| last - first + 1)
    }
}

/// What to send of an object of `size` bytes: the honoured range, or the whole
/// object. `None` when the range cannot be satisfied.
fn plan_span(range: Option<ByteRange>, size: u64) -> Option<Span> {
    let Some(range) = range else {
        let whole = size.checked_sub(1).map(|last| (0, last));

        return Some(Span::new(whole, false));
    };

    range
        .resolve(size)
        .map(|offsets| Span::new(Some(offsets), true))
}

/// The slice one streamed chunk covers, checked against what the backend
/// returned: the same offsets, the expected length, and — when the backend
/// reports entity tags — the tag the response was committed to.
fn verify_chunk(
    object: Option<RangedObject>,
    offsets: (u64, u64),
    etag: Option<&str>,
) -> io::Result<Bytes> {
    let Some(object) = object else {
        return Err(io::Error::other("stored object shrank while streaming"));
    };

    if let (Some(expected), Some(actual)) = (etag, object.etag.as_deref())
        && expected != actual
    {
        return Err(io::Error::other("stored object changed while streaming"));
    }

    let expected_len = offsets.1 - offsets.0 + 1;
    if object.range != Some(offsets) || u64::try_from(object.data.len()).ok() != Some(expected_len)
    {
        return Err(io::Error::other(
            "storage backend returned a different slice",
        ));
    }

    Ok(Bytes::from(object.data))
}

/// What every chunk read of one streamed body needs; shared by the reads,
/// since the body outlives the handler.
struct ChunkSource {
    storage: SharedStorage,
    key: String,
    etag: Option<String>,
}

/// Read one chunk of the object.
async fn read_chunk(source: Arc<ChunkSource>, offsets: (u64, u64)) -> io::Result<Bytes> {
    let range = ByteRange::inclusive(offsets.0, offsets.1);

    let object = read_blocking(&source.storage, source.key.clone(), Some(range))
        .await
        .map_err(io::Error::other)?;

    verify_chunk(object, offsets, source.etag.as_deref())
}

/// The inclusive offsets of the chunks `first..=last` is read in, each at
/// most [`STREAM_CHUNK_BYTES`] long.
fn chunk_offsets(first: u64, last: u64) -> impl Iterator<Item = (u64, u64)> {
    let mut next = Some(first);

    iter::from_fn(move || {
        let start = next.filter(|start| *start <= last)?;
        let end = last.min(start.saturating_add(STREAM_CHUNK_BYTES - 1));

        next = end.checked_add(1);

        Some((start, end))
    })
}

/// A body that streams `span` of the object in bounded chunks. Each chunk is
/// read only when the connection polls for it: the next read waits until the
/// client has taken the previous chunk, and a body that is never polled (a
/// `HEAD`) or dropped (a client that went away) reads nothing further. A
/// failed read is the stream's error, which aborts the response rather than
/// truncating it silently.
fn streamed_body(req: &RemoteRequest<'_>, meta: &ObjectMeta, span: &Span) -> Body {
    let Some((first, last)) = span.offsets else {
        return Body::empty();
    };

    let source = Arc::new(ChunkSource {
        storage: req.storage.clone(),
        key: req.key.to_string(),
        etag: meta.etag.clone(),
    });

    let chunks = stream_iter(chunk_offsets(first, last))
        .then(move |offsets| read_chunk(Arc::clone(&source), offsets));

    Body::from_stream(chunks)
}

/// Answer from the backend's metadata, streaming the body.
fn serve_streamed(req: &RemoteRequest<'_>, meta: &ObjectMeta, size: u64) -> Response {
    let etag = remote_etag(req.key, meta);
    let last_modified = meta.last_modified.as_deref();

    if !preconditions_pass(req.conditional, Some(&etag), last_modified) {
        return precondition_failed(req.headers);
    }

    if is_fresh(req.conditional, &etag, last_modified) {
        return not_modified(&etag, last_modified, req.headers);
    }

    let range = parse_range(req.conditional.range.as_ref())
        .filter(|_| if_range_allows(req.conditional, Some(&etag), last_modified));

    let Some(span) = plan_span(range, size) else {
        return unsatisfiable(Some(size), req.headers);
    };

    let status = if span.partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    let mut builder = with_validators(status, &etag, last_modified)
        .header(CONTENT_TYPE, req.headers.mime())
        .header(CONTENT_LENGTH, span.len());

    if let (true, Some(offsets)) = (span.partial, span.offsets) {
        builder = builder.header(CONTENT_RANGE, content_range(offsets, Some(size)));
    }

    finish(builder.body(streamed_body(req, meta, &span)), req.headers)
}

/// Answer from one read of a backend that reports no metadata. The object's
/// validators are only known once it has been read, so a range guarded by
/// `If-Range` reads the whole object and slices it here once the guard is
/// decided.
async fn serve_buffered(req: &RemoteRequest<'_>) -> Response {
    let range = parse_range(req.conditional.range.as_ref());
    let guarded = range.is_some() && req.conditional.if_range.is_some();
    let read_range = if guarded { None } else { range };

    let object = match read_blocking(req.storage, req.key.to_string(), read_range).await {
        Ok(Some(object)) => object,
        Ok(None) => return unsatisfiable(None, req.headers),
        Err(e) => return failure(&e),
    };

    let meta = object.meta();
    let etag = remote_etag(req.key, &meta);
    let last_modified = meta.last_modified.as_deref();

    if !preconditions_pass(req.conditional, Some(&etag), last_modified) {
        return precondition_failed(req.headers);
    }

    if is_fresh(req.conditional, &etag, last_modified) {
        return not_modified(&etag, last_modified, req.headers);
    }

    let Some(range) = range.filter(|_| guarded) else {
        return buffered_response(&etag, last_modified, object, req.headers);
    };

    if !if_range_allows(req.conditional, Some(&etag), last_modified) {
        return buffered_response(&etag, last_modified, object, req.headers);
    }

    match slice_locally(&object.data, &range) {
        Some(slice) => buffered_response(&etag, last_modified, slice, req.headers),
        None => unsatisfiable(meta.size, req.headers),
    }
}

/// `200` with the whole object, or `206` with the slice that was read.
fn buffered_response(
    etag: &str,
    last_modified: Option<&str>,
    object: RangedObject,
    headers: &ServeHeaders<'_>,
) -> Response {
    let range = object
        .range
        .map(|offsets| content_range(offsets, object.total_size));

    let status = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    let mut builder =
        with_validators(status, etag, last_modified).header(CONTENT_TYPE, headers.mime());

    if let Some(range) = range.as_deref() {
        builder = builder.header(CONTENT_RANGE, range);
    }

    finish(builder.body(Body::from(object.data)), headers)
}

/// Serve `key` from a remote backend, honouring the request's `Range`,
/// `If-Range` and conditional headers.
pub(super) async fn serve_remote(
    storage: &SharedStorage,
    key: &str,
    conditional: &ConditionalHeaders,
    headers: &ServeHeaders<'_>,
) -> Response {
    let req = RemoteRequest {
        storage,
        key,
        conditional,
        headers,
    };

    match stat_blocking(storage, key.to_string()).await {
        Ok(Some(meta)) => match meta.size {
            Some(size) => serve_streamed(&req, &meta, size),
            None => serve_buffered(&req).await,
        },
        Ok(None) => serve_buffered(&req).await,
        Err(e) => failure(&e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use anyhow::Result;
    use axum::http::{
        HeaderValue,
        header::{ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION},
    };
    use http_body_util::BodyExt as _;
    use mlua::Lua;

    use super::*;
    use crate::core::{
        LocalLease,
        upload::{StorageBackend, storage::CustomStorage},
    };

    /// An S3-like backend: it reports metadata and serves true ranged reads,
    /// counting every byte it hands out and the largest single read.
    struct StreamingRemote {
        data: Vec<u8>,
        fetched: AtomicU64,
        largest_read: AtomicU64,
        reads: AtomicU64,
        /// Report a different entity tag from the second read on — the object
        /// was replaced while streaming.
        replaced_mid_stream: bool,
    }

    impl StreamingRemote {
        fn new(data: &[u8]) -> Arc<Self> {
            Arc::new(Self {
                data: data.to_vec(),
                fetched: AtomicU64::new(0),
                largest_read: AtomicU64::new(0),
                reads: AtomicU64::new(0),
                replaced_mid_stream: false,
            })
        }

        fn fetched(&self) -> u64 {
            self.fetched.load(Ordering::SeqCst)
        }
    }

    impl StorageBackend for StreamingRemote {
        fn put(&self, _key: &str, _data: &[u8], _content_type: &str) -> Result<()> {
            Ok(())
        }

        fn get(&self, _key: &str) -> Result<Vec<u8>> {
            panic!("a metadata-reporting backend must never be read whole");
        }

        fn get_range(&self, _key: &str, range: Option<ByteRange>) -> Result<Option<RangedObject>> {
            let range = range.expect("streamed reads are always ranged");
            let total = u64::try_from(self.data.len()).unwrap();
            let Some((first, last)) = range.resolve(total) else {
                return Ok(None);
            };

            let slice = self.data[usize::try_from(first).unwrap()..=usize::try_from(last).unwrap()]
                .to_vec();
            let len = u64::try_from(slice.len()).unwrap();
            self.fetched.fetch_add(len, Ordering::SeqCst);
            self.largest_read.fetch_max(len, Ordering::SeqCst);

            let read = self.reads.fetch_add(1, Ordering::SeqCst);
            let etag = if self.replaced_mid_stream && read > 0 {
                "v2"
            } else {
                "v1"
            };

            Ok(Some(
                RangedObject::whole(slice)
                    .total_size(Some(total))
                    .range(Some((first, last)))
                    .etag(Some(etag.to_string()))
                    .build(),
            ))
        }

        fn stat(&self, _key: &str) -> Result<Option<ObjectMeta>> {
            Ok(Some(
                ObjectMeta::builder()
                    .size(Some(u64::try_from(self.data.len()).unwrap()))
                    .etag(Some("v1".to_string()))
                    .last_modified(Some("Sun, 06 Nov 1994 08:49:37 GMT".to_string()))
                    .build(),
            ))
        }

        fn delete(&self, _key: &str) -> Result<()> {
            Ok(())
        }

        fn exists(&self, _key: &str) -> Result<bool> {
            Ok(true)
        }

        fn kind(&self) -> &'static str {
            "streaming-remote"
        }
    }

    async fn serve_streaming(
        backend: &Arc<StreamingRemote>,
        cond: &ConditionalHeaders,
    ) -> Response {
        let storage: SharedStorage = backend.clone();

        serve_remote(&storage, "media/a.pdf", cond, &headers()).await
    }

    /// Regression: a remote object was loaded whole into memory for every
    /// request. The body is now streamed in bounded ranged reads.
    #[tokio::test]
    async fn a_whole_object_is_streamed_in_bounded_reads() {
        let backend = StreamingRemote::new(b"0123456789");

        let response = serve_streaming(&backend, &conditional(None, None)).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(CONTENT_LENGTH).unwrap(), "10");
        assert_eq!(response.headers().get(ETAG).unwrap(), "\"v1\"");
        assert_eq!(body_of(response).await, b"0123456789");
        assert_eq!(backend.fetched(), 10);
        assert!(backend.largest_read.load(Ordering::SeqCst) <= STREAM_CHUNK_BYTES);
    }

    /// Regression: freshness was decided after the body was downloaded. A
    /// conditional hit is now answered from metadata alone.
    #[tokio::test]
    async fn a_conditional_hit_transfers_no_bytes() {
        let backend = StreamingRemote::new(b"0123456789");

        let response = serve_streaming(&backend, &conditional(None, Some("\"v1\""))).await;

        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert!(body_of(response).await.is_empty());
        assert_eq!(backend.fetched(), 0);
    }

    #[tokio::test]
    async fn an_open_ended_range_streams_to_the_end() {
        let backend = StreamingRemote::new(b"0123456789");

        let response = serve_streaming(&backend, &conditional(Some("bytes=3-"), None)).await;

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(CONTENT_RANGE).unwrap(),
            "bytes 3-9/10"
        );
        assert_eq!(response.headers().get(CONTENT_LENGTH).unwrap(), "7");
        assert_eq!(body_of(response).await, b"3456789");
        assert_eq!(backend.fetched(), 7);
    }

    #[tokio::test]
    async fn a_streamed_suffix_range_returns_the_tail() {
        let backend = StreamingRemote::new(b"0123456789");

        let response = serve_streaming(&backend, &conditional(Some("bytes=-3"), None)).await;

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(CONTENT_RANGE).unwrap(),
            "bytes 7-9/10"
        );
        assert_eq!(body_of(response).await, b"789");
    }

    #[tokio::test]
    async fn a_streamed_unsatisfiable_range_is_416_with_the_size() {
        let backend = StreamingRemote::new(b"0123456789");

        let response = serve_streaming(&backend, &conditional(Some("bytes=10-"), None)).await;

        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers().get(CONTENT_RANGE).unwrap(), "bytes */10");
        assert_eq!(backend.fetched(), 0);
    }

    /// A range whose `If-Range` names an older version is ignored: the whole
    /// current object is served, never a slice of it spliced onto the old one.
    #[tokio::test]
    async fn a_stale_if_range_serves_the_whole_object() {
        let backend = StreamingRemote::new(b"0123456789");
        let cond = ConditionalHeaders {
            if_range: Some("\"v0\"".parse().unwrap()),
            ..conditional(Some("bytes=2-4"), None)
        };

        let response = serve_streaming(&backend, &cond).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_of(response).await, b"0123456789");
    }

    #[tokio::test]
    async fn a_matching_if_range_serves_the_slice() {
        let backend = StreamingRemote::new(b"0123456789");
        let cond = ConditionalHeaders {
            if_range: Some("\"v1\"".parse().unwrap()),
            ..conditional(Some("bytes=2-4"), None)
        };

        let response = serve_streaming(&backend, &cond).await;

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(body_of(response).await, b"234");
    }

    /// A failed `If-Match` / `If-Unmodified-Since` is a `412` judged from the
    /// metadata — before the range, without transferring a byte; a matching
    /// `If-Match` serves the range.
    #[tokio::test]
    async fn preconditions_are_judged_from_metadata_before_the_range() {
        let backend = StreamingRemote::new(b"0123456789");
        let pinned = |tag: &str| ConditionalHeaders {
            if_match: Some(tag.parse().unwrap()),
            ..conditional(Some("bytes=2-4"), None)
        };
        let stale_date = ConditionalHeaders {
            if_unmodified_since: Some("Sat, 05 Nov 1994 08:49:37 GMT".parse().unwrap()),
            ..conditional(Some("bytes=2-4"), None)
        };

        for cond in [pinned("\"v0\""), pinned("W/\"v1\""), stale_date] {
            let refused = serve_streaming(&backend, &cond).await;
            assert_eq!(refused.status(), StatusCode::PRECONDITION_FAILED);
        }
        assert_eq!(backend.fetched(), 0);

        let current = serve_streaming(&backend, &pinned("\"v1\"")).await;
        assert_eq!(current.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(body_of(current).await, b"234");
    }

    /// An object replaced while its body streams aborts the transfer instead
    /// of splicing the new version onto the old.
    #[tokio::test]
    async fn an_object_replaced_mid_stream_aborts_the_body() {
        let backend = Arc::new(StreamingRemote {
            replaced_mid_stream: true,
            ..Arc::into_inner(StreamingRemote::new(b"0123456789")).unwrap()
        });

        let response = serve_streaming(&backend, &conditional(None, None)).await;

        assert!(response.into_body().collect().await.is_err());
    }

    #[tokio::test]
    async fn an_empty_object_is_an_empty_200() {
        let backend = StreamingRemote::new(b"");

        let response = serve_streaming(&backend, &conditional(None, None)).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(CONTENT_LENGTH).unwrap(), "0");
        assert!(body_of(response).await.is_empty());
    }

    #[test]
    fn chunk_offsets_cover_the_span_in_bounded_chunks() {
        let chunks: Vec<_> = chunk_offsets(0, 9).collect();
        assert_eq!(chunks, vec![(0, 3), (4, 7), (8, 9)]);

        assert_eq!(chunk_offsets(5, 5).collect::<Vec<_>>(), vec![(5, 5)]);
        assert_eq!(
            chunk_offsets(u64::MAX - 1, u64::MAX).collect::<Vec<_>>(),
            vec![(u64::MAX - 1, u64::MAX)]
        );
    }

    /// Regression: the body was pumped by a spawned task that read ahead as
    /// soon as the response was built, so a `HEAD` (whose body is never
    /// polled) or a client that went away still fetched chunks from the
    /// remote. Reads now happen only as the body is polled.
    #[tokio::test]
    async fn chunks_are_read_only_as_the_body_is_polled() {
        let backend = StreamingRemote::new(b"0123456789");

        let unpolled = serve_streaming(&backend, &conditional(None, None)).await;
        task::yield_now().await;
        drop(unpolled);
        assert_eq!(backend.reads.load(Ordering::SeqCst), 0);

        let response = serve_streaming(&backend, &conditional(None, None)).await;
        let mut body = response.into_body();
        let first = body.frame().await.expect("a frame").expect("a chunk");
        assert_eq!(first.into_data().unwrap(), b"0123"[..]);
        drop(body);

        assert_eq!(backend.reads.load(Ordering::SeqCst), 1);
    }

    /// A custom Lua backend with `stat` and `get_range` handlers is served
    /// like S3: from metadata, streamed in ranged reads, never through `get`.
    #[tokio::test]
    async fn a_custom_backend_with_ranged_handlers_is_streamed() {
        let lua = Lua::new();
        lua.load(
            r#"
            crap = { _storage = {}, _gets = 0, _ranges = 0 }
            local data = "0123456789"

            crap._storage.put = function() end
            crap._storage.delete = function() end
            crap._storage.get = function()
                crap._gets = crap._gets + 1
                return data
            end
            crap._storage.stat = function()
                return { size = #data, etag = "v1" }
            end
            crap._storage.get_range = function(_, first, last)
                crap._ranges = crap._ranges + 1
                return data:sub(first + 1, last + 1)
            end
            "#,
        )
        .exec()
        .unwrap();
        let storage: SharedStorage = Arc::new(CustomStorage::new(Arc::new(LocalLease::new(&lua))));

        let whole = serve_remote(
            &storage,
            "media/a.pdf",
            &conditional(None, None),
            &headers(),
        )
        .await;
        assert_eq!(whole.status(), StatusCode::OK);
        assert_eq!(whole.headers().get(ETAG).unwrap(), "\"v1\"");
        assert_eq!(body_of(whole).await, b"0123456789");

        let cached = conditional(None, Some("\"v1\""));
        let fresh = serve_remote(&storage, "media/a.pdf", &cached, &headers()).await;
        assert_eq!(fresh.status(), StatusCode::NOT_MODIFIED);

        let ranged = conditional(Some("bytes=7-"), None);
        let tail = serve_remote(&storage, "media/a.pdf", &ranged, &headers()).await;
        assert_eq!(tail.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(body_of(tail).await, b"789");

        let whole_reads: i64 = lua.load("return crap._gets").eval().unwrap();
        let range_reads: i64 = lua.load("return crap._ranges").eval().unwrap();
        assert_eq!(whole_reads, 0, "a ranged backend is never read whole");
        assert_eq!(
            range_reads, 4,
            "three chunks for the whole object, one for the tail"
        );
    }

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
            ..ConditionalHeaders::default()
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

    /// A backend without metadata honours `If-Range` too: a stale guard
    /// serves the whole object, a matching one the slice.
    #[tokio::test]
    async fn a_buffered_read_honours_if_range() {
        let storage = storage(b"0123456789");
        let first = serve_remote(
            &storage,
            "media/a.pdf",
            &conditional(None, None),
            &headers(),
        )
        .await;
        let etag = first.headers().get(ETAG).unwrap().clone();

        let guarded = |tag: HeaderValue| ConditionalHeaders {
            if_range: Some(tag),
            ..conditional(Some("bytes=2-4"), None)
        };

        let fresh = serve_remote(&storage, "media/a.pdf", &guarded(etag), &headers()).await;
        assert_eq!(fresh.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(fresh.headers().get(CONTENT_RANGE).unwrap(), "bytes 2-4/10");
        assert_eq!(body_of(fresh).await, b"234");

        let stale_tag = HeaderValue::from_static("\"stale\"");
        let stale = serve_remote(&storage, "media/a.pdf", &guarded(stale_tag), &headers()).await;
        assert_eq!(stale.status(), StatusCode::OK);
        assert_eq!(body_of(stale).await, b"0123456789");
    }

    /// A backend without metadata judges `If-Match` on the object it read.
    #[tokio::test]
    async fn a_buffered_read_honours_if_match() {
        let cond = ConditionalHeaders {
            if_match: Some(HeaderValue::from_static("\"stale\"")),
            ..conditional(Some("bytes=2-4"), None)
        };

        let response =
            serve_remote(&storage(b"0123456789"), "media/a.pdf", &cond, &headers()).await;

        assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
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
