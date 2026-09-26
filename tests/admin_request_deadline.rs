//! Request deadlines of the admin router: `[server] request_timeout` bounds
//! every request, including the body the CSRF check buffers; the routes that
//! accept a file into an upload collection follow `[server] upload_timeout`
//! instead.

mod admin_collections_support;

use std::{fs, io, net::SocketAddr, time::Duration};

use axum::{
    body::{Body, Bytes},
    extract::ConnectInfo,
    http::{Request, Response, StatusCode},
};
use tokio::time::{sleep, timeout};
use tokio_stream::{StreamExt, iter, pending};
use tower::ServiceExt;

use crap_cms::{
    config::CrapConfig,
    core::{
        HookRef, auth,
        collection::{Activation, AuthMethod, CollectionDefinition, SurfaceSet},
        upload::CollectionUpload,
    },
    db::{DbConnection, query},
};

use admin_collections_support::{
    TEST_CSRF, TestApp, auth_and_csrf, create_test_user, make_auth_cookie, make_posts_def,
    make_users_def, setup_app_with_config,
};

/// How long a test waits for a response before concluding the server is
/// still reading the stalled body.
const WAIT: Duration = Duration::from_secs(5);

fn make_media_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("media");
    def.upload = Some(CollectionUpload::new());

    def
}

fn config(request_timeout: u64, upload_timeout: u64) -> CrapConfig {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.server.request_timeout = request_timeout;
    config.server.upload_timeout = upload_timeout;

    config
}

fn app(request_timeout: u64, upload_timeout: u64) -> TestApp {
    setup_app_with_config(
        vec![make_users_def(), make_posts_def(), make_media_def()],
        vec![],
        config(request_timeout, upload_timeout),
    )
}

/// A body that sends `head` and then never another byte, nor its end.
fn stalled_body(head: &'static [u8]) -> Body {
    let stream = iter([Ok::<_, io::Error>(Bytes::from_static(head))]).chain(pending());

    Body::from_stream(stream)
}

fn bearer(app: &TestApp) -> String {
    let user_id = create_test_user(app, "uploader@test.com", "secret123");

    let conn = app.pool.get().unwrap();
    let session_version = query::auth::get_session_version(&conn, "users", &user_id).unwrap();
    drop(conn);

    let claims = auth::Claims::builder(user_id.as_str(), "users")
        .email("uploader@test.com")
        .session_version(session_version)
        .exp(u64::try_from(chrono::Utc::now().timestamp()).unwrap() + 3600)
        .build()
        .unwrap();

    format!(
        "Bearer {}",
        auth::create_token(&claims, app.jwt_secret.as_ref()).unwrap()
    )
}

/// A stalled, bearer-authenticated multipart POST to `path`.
fn stalled_upload(app: &TestApp, path: &str) -> Request<Body> {
    Request::post(path)
        .header("content-type", "multipart/form-data; boundary=b")
        .header("authorization", bearer(app))
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
        .body(stalled_body(b"--b\r\n"))
        .unwrap()
}

/// A stalled, bearer-authenticated urlencoded form POST to `path` — the body
/// a collection without uploads accepts. The CSRF token rides in the header,
/// so the stall is met by the handler's form read, not the CSRF check.
fn stalled_form(app: &TestApp, path: &str) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", bearer(app))
        .header("Cookie", format!("crap_csrf={TEST_CSRF}"))
        .header("x-csrf-token", TEST_CSRF)
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
        .body(stalled_body(b"title=a"))
        .unwrap()
}

/// Send `request`, or `None` when no response came within [`WAIT`].
async fn send(app: TestApp, request: Request<Body>) -> Option<Response<Body>> {
    timeout(WAIT, app.router.oneshot(request))
        .await
        .ok()
        .map(Result::unwrap)
}

/// Regression: the admin server had no request deadline by default, and the
/// optional one sat inside nothing the CSRF check could not outrun — a login
/// form trickled at the server held its connection for as long as the client
/// liked. The CSRF check buffers this body, so it is cut there.
#[tokio::test]
async fn a_stalled_form_body_is_cut_at_the_request_timeout() {
    let app = app(1, 0);

    let request = Request::post("/admin/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("Cookie", format!("crap_csrf={TEST_CSRF}"))
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
        .body(stalled_body(b"collection=users&email=a"))
        .unwrap();

    let response = send(app, request).await.expect("cut by the deadline");

    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
}

/// A file on a slow link takes long: with `upload_timeout = 0` an upload is
/// not cut by the (much shorter) request deadline.
#[tokio::test]
async fn a_stalled_upload_is_not_cut_at_the_request_timeout() {
    let app = app(1, 0);
    let request = stalled_upload(&app, "/api/upload/media");

    assert!(
        send(app, request).await.is_none(),
        "the upload is still being read"
    );
}

#[tokio::test]
async fn a_stalled_upload_is_cut_at_the_upload_timeout() {
    let app = app(60, 1);
    let request = stalled_upload(&app, "/admin/collections/media");

    let response = send(app, request).await.expect("cut by the deadline");

    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
}

/// The upload deadline follows the target collection: a collection without
/// uploads keeps the request deadline on the same route. The body is the
/// urlencoded form such a collection reads — a multipart body is refused
/// (`422`) before any of it is read, so it could never show the deadline.
#[tokio::test]
async fn a_route_into_a_collection_without_uploads_keeps_the_request_timeout() {
    let app = app(1, 0);
    let request = stalled_form(&app, "/admin/collections/posts");

    let response = send(app, request).await.expect("cut by the deadline");

    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
}

/// A `before_change` hook that keeps the save busy for two to three seconds
/// of wall time (`os.time` ticks in whole seconds).
const SLOW_HOOK: &str = "local M = {}
function M.wait(ctx)
    local started = os.time()
    while os.time() - started < 3 do end
    return ctx
end
return M
";

/// Rows in `posts`.
fn post_count(app: &TestApp) -> i64 {
    let conn = app.pool.get().unwrap();

    conn.query_one("SELECT COUNT(*) AS c FROM posts", &[])
        .unwrap()
        .unwrap()
        .get_i64("c")
        .unwrap()
}

/// Regression: the `408` raced only the handler future, while the save ran
/// on an uncancellable blocking thread and committed afterwards — the editor,
/// told the save timed out, re-submitted and created a duplicate. The save
/// that overruns its request's deadline now rolls back, so after a `408` no
/// document exists, even once the hook has long finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_answered_408_commits_nothing() {
    let mut posts = make_posts_def();
    posts.hooks.before_change = vec!["hooks.slow.wait".into()];

    let mut slow_config = config(1, 0);
    slow_config.hooks.max_instructions = 0;

    let app = setup_app_with_config(vec![make_users_def(), posts], vec![], slow_config);

    let hooks_dir = app.tmp.path().join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(hooks_dir.join("slow.lua"), SLOW_HOOK).unwrap();

    let user_id = create_test_user(&app, "slow_save@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "slow_save@test.com");

    let request = Request::post("/admin/collections/posts")
        .header("cookie", auth_and_csrf(&cookie))
        .header("X-CSRF-Token", TEST_CSRF)
        .header("content-type", "application/x-www-form-urlencoded")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
        .body(Body::from("title=Late"))
        .unwrap();

    let response = timeout(WAIT, app.router.clone().oneshot(request))
        .await
        .expect("answered")
        .unwrap();

    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);

    // Let the hook finish and the save reach its commit.
    sleep(Duration::from_secs(4)).await;

    assert_eq!(post_count(&app), 0, "the timed-out save committed nothing");
}

/// A per-request auth strategy that provisions something (a post, standing in
/// for a first-sight user) and then keeps the request busy for two to three
/// seconds of wall time before naming its user.
const SLOW_STRATEGY: &str = r#"
return function(ctx)
    local uid = ctx.headers["x-sso-user"]
    if not uid then return nil end
    crap.collections.create("posts", { title = "provisioned" })
    local started = os.time()
    while os.time() - started < 3 do end
    return { id = uid }
end
"#;

/// Regression: the account flows — auth strategies, auth callbacks, login,
/// logout, MFA, password reset, email verification — ran their writes outside
/// the request's commit gate, so a request answered `408` could still change
/// an account (provision a user, consume a token) a moment later. A strategy
/// whose write reaches its commit after the deadline now rolls back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_auth_strategy_answered_408_commits_nothing() {
    let mut users = make_users_def();
    users
        .auth
        .as_mut()
        .unwrap()
        .methods
        .push(AuthMethod::Strategy {
            name: "sso".into(),
            authenticate: HookRef::new("auth.sso"),
            activates_on: Activation::header("x-sso-user"),
            surfaces: SurfaceSet::admin_only(),
        });

    let mut slow_config = config(1, 0);
    slow_config.hooks.max_instructions = 0;

    let app = setup_app_with_config(vec![users, make_posts_def()], vec![], slow_config);

    let auth_dir = app.tmp.path().join("auth");
    fs::create_dir_all(&auth_dir).unwrap();
    fs::write(auth_dir.join("sso.lua"), SLOW_STRATEGY).unwrap();

    let user_id = create_test_user(&app, "slow_sso@test.com", "pass123");

    let request = Request::get("/admin")
        .header("x-sso-user", &user_id)
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
        .body(Body::empty())
        .unwrap();

    let response = timeout(WAIT, app.router.clone().oneshot(request))
        .await
        .expect("answered")
        .unwrap();

    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);

    // Let the strategy finish and its transaction reach its commit.
    sleep(Duration::from_secs(4)).await;

    assert_eq!(
        post_count(&app),
        0,
        "the timed-out strategy committed nothing"
    );
}
