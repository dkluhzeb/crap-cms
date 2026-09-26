//! Request deadlines: how long an admin request may take to arrive and be
//! answered.
//!
//! Every route gets `[server] request_timeout`; the routes that accept a file
//! — an upload collection's admin create / update and the `/api/upload`
//! create / update — get `[server] upload_timeout` instead, since a large file
//! on a slow link legitimately takes long. A deadline covers reading the
//! request body (including the CSRF check's form buffering) and producing the
//! response head; a streamed response body (SSE, a file download) is never
//! cut. `0` disables a deadline.
//!
//! [`request_deadline`] is applied once around the whole router, OUTSIDE the
//! other request middleware, so no layer can read a body before the clock
//! starts. The route paths it recognizes are the constants the routes are
//! registered under, so the two cannot drift apart.
//!
//! A `408` means nothing was changed. The handler's write runs on a blocking
//! thread that cannot be cancelled, so the deadline is also carried into it as
//! the request's commit gate (see [`crate::core::commit_gate`]): the write's
//! statements are bounded by the deadline, and its `COMMIT` must be admitted
//! by the gate. When the deadline passes the middleware expires the gate —
//! from then on no write of the request commits — and answers `408`, unless a
//! write had already committed, in which case it waits for the handler's own
//! answer.

use std::{
    pin::pin,
    time::{Duration, Instant},
};

use axum::{
    extract::{FromRequestParts, MatchedPath, RawPathParams, Request, State},
    http::{Method, StatusCode, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tokio::time::timeout_at;

use crate::{
    admin::{AdminState, target_slug},
    core::{CommitGate, with_commit_gate},
};

/// The admin collection route whose `POST` creates a document.
pub(crate) const COLLECTION_ROUTE: &str = "/admin/collections/{slug}";

/// The admin document route whose `POST` / `PUT` updates a document.
pub(crate) const COLLECTION_ITEM_ROUTE: &str = "/admin/collections/{slug}/{id}";

/// Where the upload API is nested in the admin router.
pub(crate) const API_PREFIX: &str = "/api";

/// The upload API route (under [`API_PREFIX`]) whose `POST` creates an upload.
pub(crate) const UPLOAD_API_ROUTE: &str = "/upload/{slug}";

/// The upload API route (under [`API_PREFIX`]) whose `PATCH` updates an upload.
pub(crate) const UPLOAD_API_ITEM_ROUTE: &str = "/upload/{slug}/{id}";

/// A configured number of seconds as a deadline; `0` is none.
fn seconds(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Whether the route `path` (as registered) takes a file with `method`.
fn accepts_file(path: &str, method: &Method) -> bool {
    match path {
        COLLECTION_ROUTE => *method == Method::POST,
        COLLECTION_ITEM_ROUTE => matches!(*method, Method::POST | Method::PUT),
        _ => path
            .strip_prefix(API_PREFIX)
            .is_some_and(|api_path| upload_api_accepts_file(api_path, method)),
    }
}

/// [`accepts_file`] for a path inside the upload API.
fn upload_api_accepts_file(path: &str, method: &Method) -> bool {
    match path {
        UPLOAD_API_ROUTE => *method == Method::POST,
        UPLOAD_API_ITEM_ROUTE => *method == Method::PATCH,
        _ => false,
    }
}

/// The deadline of a request to a route that may take a file: the upload
/// deadline when collection `slug` accepts uploads, else the request deadline
/// (a plain collection's create / update is an ordinary form).
fn file_route_deadline(state: &AdminState, slug: Option<&str>) -> Option<Duration> {
    let server = &state.config.server;

    let is_upload = slug
        .and_then(|slug| state.infra.registry.get_collection(slug))
        .is_some_and(|def| def.is_upload_collection());

    if is_upload {
        return seconds(server.upload_timeout);
    }

    seconds(server.request_timeout)
}

/// The deadline of the request whose head is `parts`.
async fn deadline_for(state: &AdminState, parts: &mut Parts) -> Option<Duration> {
    let path = parts
        .extensions
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned());

    if !path.is_some_and(|path| accepts_file(&path, &parts.method)) {
        return seconds(state.config.server.request_timeout);
    }

    let params = RawPathParams::from_request_parts(parts, &()).await.ok();
    let slug = params.as_ref().and_then(target_slug);

    file_route_deadline(state, slug)
}

/// The request's answer once its handler finished: `408` when its deadline
/// passed with nothing committed — a write that reached its commit too late
/// rolled back — else the handler's own.
fn settled(gate: &CommitGate, response: Response) -> Response {
    if gate.expired() {
        return StatusCode::REQUEST_TIMEOUT.into_response();
    }

    response
}

/// Run `request` through `next` within `limit` under the request's commit
/// gate, answering `408 Request Timeout` once it has passed with nothing
/// committed (see the module docs).
async fn run_within(limit: Option<Duration>, request: Request, next: Next) -> Response {
    // A deadline beyond what an `Instant` can hold is never reached.
    let Some(deadline) = limit.and_then(|limit| Instant::now().checked_add(limit)) else {
        return next.run(request).await;
    };

    let gate = CommitGate::new(deadline);
    let mut handler = pin!(with_commit_gate(gate.clone(), next.run(request)));

    if let Ok(response) = timeout_at(deadline.into(), &mut handler).await {
        return settled(&gate, response);
    }

    if gate.expire() {
        return StatusCode::REQUEST_TIMEOUT.into_response();
    }

    // A write already committed: "nothing happened" would be false.
    handler.await
}

/// Router middleware enforcing the request's deadline (see the module docs).
pub(crate) async fn request_deadline(
    State(state): State<AdminState>,
    request: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = request.into_parts();
    let limit = deadline_for(&state, &mut parts).await;

    run_within(limit, Request::from_parts(parts, body), next).await
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread};

    use axum::{Router, body::Body, middleware::from_fn, routing::get};
    use tokio::time::sleep;
    use tower::ServiceExt;

    use super::*;
    use crate::core::{RequestDeadlinePassed, admit_request_commit, spawn_request_blocking};
    #[cfg(feature = "sqlite")]
    use crate::{
        admin::test_state::test_admin_state_with_registry,
        core::{CollectionDefinition, Registry, upload::CollectionUpload},
    };

    #[test]
    fn file_routes_are_recognized_by_path_and_method() {
        assert!(accepts_file(COLLECTION_ROUTE, &Method::POST));
        assert!(!accepts_file(COLLECTION_ROUTE, &Method::GET));
        assert!(accepts_file(COLLECTION_ITEM_ROUTE, &Method::POST));
        assert!(accepts_file(COLLECTION_ITEM_ROUTE, &Method::PUT));
        assert!(!accepts_file(COLLECTION_ITEM_ROUTE, &Method::DELETE));
        assert!(accepts_file("/api/upload/{slug}", &Method::POST));
        assert!(accepts_file("/api/upload/{slug}/{id}", &Method::PATCH));
        assert!(!accepts_file("/api/upload/{slug}/{id}", &Method::DELETE));
        assert!(!accepts_file("/admin/login", &Method::POST));
        assert!(!accepts_file("/upload/{slug}", &Method::POST));
    }

    /// A file route takes the upload deadline only when its collection
    /// accepts uploads; a plain collection's form keeps the request deadline.
    #[cfg(feature = "sqlite")]
    #[test]
    fn only_an_upload_collection_gets_the_upload_deadline() {
        let mut media = CollectionDefinition::new("media");
        media.upload = Some(CollectionUpload::new());

        let mut registry = Registry::default();
        registry.register_collection(media);
        registry.register_collection(CollectionDefinition::new("posts"));

        let mut state = test_admin_state_with_registry(registry);
        state.config.server.request_timeout = 60;
        state.config.server.upload_timeout = 0;

        assert_eq!(file_route_deadline(&state, Some("media")), None);
        assert_eq!(
            file_route_deadline(&state, Some("posts")),
            Some(Duration::from_mins(1))
        );
        assert_eq!(
            file_route_deadline(&state, None),
            Some(Duration::from_mins(1))
        );
    }

    #[test]
    fn zero_seconds_is_no_deadline() {
        assert_eq!(seconds(0), None);
        assert_eq!(seconds(60), Some(Duration::from_mins(1)));
    }

    async fn status_within(limit: Option<Duration>, handler_takes: Duration) -> StatusCode {
        let app = Router::new()
            .route(
                "/",
                get(move || async move {
                    sleep(handler_takes).await;
                    "done"
                }),
            )
            .layer(from_fn(move |request: Request, next: Next| {
                run_within(limit, request, next)
            }));

        app.oneshot(Request::new(Body::empty()))
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn a_request_past_its_deadline_is_answered_408() {
        let status =
            status_within(Some(Duration::from_millis(20)), Duration::from_millis(500)).await;

        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
    }

    #[tokio::test]
    async fn a_request_within_its_deadline_or_without_one_completes() {
        let quick = status_within(Some(Duration::from_secs(5)), Duration::ZERO).await;
        let unbounded = status_within(None, Duration::from_millis(50)).await;

        assert_eq!(quick, StatusCode::OK);
        assert_eq!(unbounded, StatusCode::OK);
    }

    /// Serve one request whose handler is `handler`, within `limit`.
    async fn status_of<F, Fut>(limit: Duration, handler: F) -> StatusCode
    where
        F: FnOnce() -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = &'static str> + Send + 'static,
    {
        let app = Router::new().route("/", get(handler)).layer(from_fn(
            move |request: Request, next: Next| run_within(Some(limit), request, next),
        ));

        app.oneshot(Request::new(Body::empty()))
            .await
            .unwrap()
            .status()
    }

    /// Regression: the `408` raced only the handler future, while the write
    /// ran on an uncancellable blocking thread and committed afterwards — the
    /// client was told nothing happened, re-submitted, and created a
    /// duplicate. A write reaching its commit after the deadline is now
    /// refused, so the `408` holds.
    #[tokio::test]
    async fn a_write_reaching_its_commit_after_a_408_is_refused() {
        let (sent, outcome) = mpsc::channel();

        let status = status_of(Duration::from_millis(20), move || async move {
            let _ = spawn_request_blocking(move || {
                thread::sleep(Duration::from_millis(150));
                sent.send(admit_request_commit()).unwrap();
            })
            .await;

            "done"
        })
        .await;

        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(
            outcome.recv_timeout(Duration::from_secs(5)).unwrap(),
            Err(RequestDeadlinePassed),
            "the late write rolls back"
        );
    }

    /// A request whose write committed before the deadline is not answered
    /// `408` when its remaining work overruns — the change happened, so the
    /// handler's own answer is the true one.
    #[tokio::test]
    async fn a_committed_write_is_answered_by_its_handler_past_the_deadline() {
        let status = status_of(Duration::from_millis(50), || async {
            spawn_request_blocking(admit_request_commit)
                .await
                .unwrap()
                .expect("in time");

            sleep(Duration::from_millis(200)).await;

            "done"
        })
        .await;

        assert_eq!(status, StatusCode::OK);
    }

    /// A handler that returns before the timer fires, but whose write reached
    /// its commit past the deadline (and was refused), is answered `408` too;
    /// one whose write committed keeps its own answer.
    #[test]
    fn a_settled_request_answers_408_only_when_nothing_committed() {
        let late = CommitGate::new(Instant::now());
        assert!(late.admit_commit().is_err());
        assert_eq!(
            settled(&late, "error page".into_response()).status(),
            StatusCode::REQUEST_TIMEOUT
        );

        let in_time = CommitGate::new(Instant::now() + Duration::from_mins(1));
        assert!(in_time.admit_commit().is_ok());
        assert_eq!(
            settled(&in_time, "done".into_response()).status(),
            StatusCode::OK
        );
    }
}
