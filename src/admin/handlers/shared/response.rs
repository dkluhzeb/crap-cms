//! Response helpers — error pages, redirects, HTMX-aware responses, toast rendering.

use std::sync::Arc;

use std::fmt::Write as _;

use axum::{
    Extension, Json,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Serialize;
use serde_json::{Value, json, to_value};
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{BasePageContext, PageMeta, PageType, page::errors::ErrorPage},
        handlers::shared::hx::HxNav,
    },
    admin::{state::render_template, templates::render_scope::RenderScope},
    core::{
        CollectionDefinition, GlobalDefinition, auth::AuthUser, richtext::renderer::html_escape,
    },
    hooks::lifecycle::{RenderCrud, RenderInfo, RenderParams},
    service::ServiceError,
};

/// Who is viewing an authenticated admin page, and how it is being
/// requested. Bundled so [`render_page`] keeps a short parameter list.
pub struct PageRequest<'a> {
    /// htmx navigation mode — decides full document vs. `#main` fragment.
    pub hx: HxNav,
    /// The signed-in admin, when the admin UI has auth enabled.
    /// `before_render` hooks read the database as this user, so anything a
    /// hook injects is already scoped to what the viewer may see.
    pub user: Option<&'a Extension<AuthUser>>,
}

impl<'a> PageRequest<'a> {
    #[must_use]
    pub fn new(hx: HxNav, user: Option<&'a Extension<AuthUser>>) -> Self {
        Self { hx, user }
    }
}

/// Render one of the built-in error pages, running `before_render` without
/// database access first. A hook can still add a banner or branding to a
/// 403/404/500; it cannot make the page depend on a database that may be
/// the very thing that failed.
fn render_error_page(state: &AdminState, template: &str, data: Value) -> Result<String, String> {
    on_blocking_section(|| {
        let data = hook_without_crud(state, template, data);
        let _scope = RenderScope::enter(RenderCrud::none());

        state.render(template, &data)
    })
}

/// Run a synchronous section that may block (a Lua VM acquire of up to
/// 5s, a `crap.http` blocking call, pooled DB work) WITHOUT parking an
/// async worker thread (ledger class L12). Converts the current
/// multi-thread-runtime worker via `block_in_place`; outside such a
/// runtime (unit tests, `current_thread`) it runs the closure inline.
/// Shared by the auth/error page renders and other admin handlers that
/// do inline Lua/DB work in an `async fn` body.
pub(crate) fn on_blocking_section<T>(f: impl FnOnce() -> T) -> T {
    use tokio::runtime::{Handle, RuntimeFlavor};

    match Handle::try_current() {
        Ok(h) if h.runtime_flavor() == RuntimeFlavor::MultiThread => tokio::task::block_in_place(f),
        _ => f(),
    }
}

/// Run `before_render` with **no** database access, for pages that have no
/// signed-in user to scope reads by: the unauthenticated auth pages and the
/// error pages. See [`RenderCrud::None`] for why that is a boundary rather
/// than an optimization.
fn hook_without_crud(state: &AdminState, template: &str, data: Value) -> Value {
    let info = RenderInfo::from_context(template, &data);

    state.infra.hook_runner.run_before_render(RenderParams {
        context: data,
        info,
        crud: RenderCrud::None {
            user: None,
            ui_locale: None,
        },
    })
}

/// Serialize a typed page-context struct, run the `before_render` Lua hook,
/// and render the named template. On render failure logs the error and
/// returns a generic fallback page.
///
/// This is the seam between typed Rust page contexts and the JSON-shaped
/// world the Lua hook + Handlebars renderer operate in.
/// `hx` decides partial-vs-full: an htmx navigation targeting `#main`
/// (see [`HxNav`]) gets only `<title>` + main content — the `htmx_partial`
/// branch in `layout/base.hbs` — everything else the full document.
///
/// **Callers must not still be holding a pooled connection here.** Lua reads
/// during this await — the `before_render` hook, and any `{{data "name"}}`
/// function the template evaluates — each acquire a read connection, so a
/// handler that keeps one alive across it puts them in competition for the
/// same pool; enough concurrent renders and every one of them waits out the
/// connection timeout. Confine database work to a helper that returns owned
/// data, the way every page handler in this module does.
pub async fn render_page<T: Serialize>(
    state: &AdminState,
    req: PageRequest<'_>,
    template: &str,
    ctx: &T,
) -> Response {
    let mut data = to_value(ctx).expect("admin page context serializes infallibly");

    if req.hx.partial
        && let Some(obj) = data.as_object_mut()
    {
        obj.insert("htmx_partial".to_string(), Value::Bool(true));
    }

    let Some(crud) = render_access(state, req.user) else {
        return render_or_error(state, template, &data);
    };

    match render_blocking(state, template.to_string(), data, crud).await {
        Ok(html) => Html(html).into_response(),
        Err(RenderFailure::Template(e)) => {
            error!("Template render error: {e}");

            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
                .into_response()
        }
        Err(RenderFailure::TaskDied) => server_error(state, "Page rendering failed"),
    }
}

/// The access level an authenticated page render runs under.
///
/// `None` means no Lua takes part in this render at all — no `before_render`
/// hook and no registered `crap.template_data` function — so the caller can
/// render inline and skip the blocking hand-off entirely.
fn render_access(state: &AdminState, user: Option<&Extension<AuthUser>>) -> Option<RenderCrud> {
    let runner = &state.infra.hook_runner;

    if !runner.has_registered_hooks_for("before_render") && !runner.has_template_data() {
        return None;
    }

    Some(RenderCrud::ReadOnly {
        pool: state.infra.pool.clone(),
        user: user.map(|Extension(au)| Arc::new(au.user_doc.clone())),
        ui_locale: user.map(|Extension(au)| au.ui_locale.clone()),
    })
}

/// Why an authenticated render produced no HTML.
enum RenderFailure {
    /// Handlebars refused the template — same fallback as the sync path.
    Template(String),
    /// The blocking task itself panicked. The context was moved into it and
    /// cannot be recovered, so the caller renders an error page rather than
    /// a page built from a hollow context.
    TaskDied,
}

/// Run `before_render` and render the template, both on a blocking thread.
///
/// They belong on the *same* hop. Lua can reach the database from either
/// side of it — the hook directly, a `{{data "name"}}` function from inside
/// Handlebars — so neither may run on an async worker. Sharing one hop also
/// means the template-data functions see exactly the access level the hook
/// saw, installed once for the whole render by [`RenderScope`].
async fn render_blocking(
    state: &AdminState,
    template: String,
    data: Value,
    crud: RenderCrud,
) -> Result<String, RenderFailure> {
    let infra = Arc::clone(&state.infra);
    let handlebars = Arc::clone(&state.handlebars);
    let info = RenderInfo::from_context(&template, &data);

    let rendered = tokio::task::spawn_blocking(move || {
        let data = infra.hook_runner.run_before_render(RenderParams {
            context: data,
            info,
            crud: crud.clone(),
        });

        let _scope = RenderScope::enter(crud);

        render_template(&handlebars, &template, &data)
    })
    .await;

    match rendered {
        Ok(Ok(html)) => Ok(html),
        Ok(Err(e)) => Err(RenderFailure::Template(e)),
        Err(e) => {
            error!("admin render task failed: {e}");

            Err(RenderFailure::TaskDied)
        }
    }
}

/// Render an unauthenticated page (login, forgot/reset password, MFA).
///
/// Always a full document — these pages are never htmx fragments — and the
/// `before_render` hook runs without database access.
pub fn render_auth_page<T: Serialize>(state: &AdminState, template: &str, ctx: &T) -> Response {
    let data = to_value(ctx).expect("admin page context serializes infallibly");

    on_blocking_section(|| {
        let data = hook_without_crud(state, template, data);

        // Declared, not merely absent: a `{{data "name"}}` function on an
        // unauthenticated page gets no database, the same as the hook above.
        let _scope = RenderScope::enter(RenderCrud::none());

        render_or_error(state, template, &data)
    })
}

/// Render a 403 Forbidden page with the given message.
///
/// The response carries the message both in the rendered HTML body (for
/// direct browser navigations, which render the 403 page) and in the
/// `X-Crap-Toast` header (for htmx form submits, which by default don't
/// swap on 4xx — `static/components/toast.js` picks the header up on
/// `htmx:afterRequest` and surfaces the message as an inline toast). Without
/// the toast header htmx form submits to access-denied paths look silently
/// broken to the user.
pub fn forbidden(state: &AdminState, message: &str) -> Response {
    let ctx = ErrorPage {
        base: BasePageContext::for_handler(
            state,
            None,
            None,
            PageMeta::new(PageType::Error403, "forbidden_page_title"),
        ),
        message: message.to_string(),
    };

    let data = to_value(&ctx).expect("ErrorPage serializes infallibly");

    let html = match render_error_page(state, "errors/403", data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!(
            "<h1>403 Forbidden</h1><p>{}</p>",
            html_escape(message)
        )),
    };

    let mut resp = (StatusCode::FORBIDDEN, html).into_response();

    let toast = json!({ "message": message, "type": "error" }).to_string();
    if let Ok(val) = toast.parse() {
        resp.headers_mut().insert("X-Crap-Toast", val);
    }

    resp
}

/// Create a redirect response to the given URL (303 See Other).
pub fn redirect_response(url: &str) -> Response {
    Redirect::to(url).into_response()
}

/// Create an HTMX-aware redirect: returns 200 + `HX-Redirect` header so HTMX does a full
/// page navigation instead of an in-place body swap.
pub fn htmx_redirect(url: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("HX-Redirect", url)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| Redirect::to(url).into_response())
}

/// Like `htmx_redirect`, but also includes `X-Created-Id` and `X-Created-Label`
/// headers so inline create panels can identify the newly created document.
/// The label is percent-encoded to safely handle non-ASCII characters in HTTP headers.
pub fn htmx_redirect_with_created(url: &str, id: &str, label: &str) -> Response {
    let encoded_label = percent_encode_header(label);

    Response::builder()
        .status(StatusCode::OK)
        .header("HX-Redirect", url)
        .header("X-Created-Id", id)
        .header("X-Created-Label", &encoded_label)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| Redirect::to(url).into_response())
}

/// Inline-create success response — used when the request carried
/// `X-Inline-Create: 1`, indicating it came from `<crap-create-panel>`.
/// The client wants to *stay on the parent page*, fire its `onCreated`
/// callback with the new id/label, and close the panel — emphatically
/// **not** follow `HX-Redirect`. We strip the redirect header and
/// return only the create-identification headers; the htmx
/// `htmx:beforeSwap` listener on the panel form sees `X-Created-Id`,
/// suppresses the swap of the empty body, and the `htmx:afterRequest`
/// listener fires the close + callback.
pub fn htmx_inline_created(id: &str, label: &str) -> Response {
    let encoded_label = percent_encode_header(label);

    Response::builder()
        .status(StatusCode::OK)
        .header("X-Created-Id", id)
        .header("X-Created-Label", &encoded_label)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| {
            // No fallback URL to redirect to — return an empty 200.
            // Caller's create-panel listener will not fire close, but
            // the document was created server-side.
            (StatusCode::OK, axum::body::Body::empty()).into_response()
        })
}

/// Percent-encode a string so it is safe for HTTP header values.
/// Non-ASCII bytes and control characters are encoded as `%XX`.
fn percent_encode_header(s: &str) -> String {
    let mut out = String::with_capacity(s.len());

    for b in s.bytes() {
        if b.is_ascii_graphic() || b == b' ' {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }

    out
}

/// Render a typed page context with an `X-Crap-Toast` header attached for
/// client-side notification. The typed context is serialized + run through
/// the `before_render` hook before rendering.
pub async fn page_with_toast<T: Serialize>(
    state: &AdminState,
    user: Option<&Extension<AuthUser>>,
    template: &str,
    ctx: &T,
    toast: &str,
) -> Response {
    let data = to_value(ctx).expect("page context serializes infallibly");

    let Some(crud) = render_access(state, user) else {
        return html_with_toast(state, template, &data, toast);
    };

    match render_blocking(state, template.to_string(), data, crud).await {
        Ok(html) => with_toast_header(Html(html).into_response(), toast),
        Err(RenderFailure::Template(e)) => {
            error!("Template render error: {e}");

            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
                .into_response()
        }
        Err(RenderFailure::TaskDied) => server_error(state, "Page rendering failed"),
    }
}

/// Render a template and set the X-Crap-Toast header for client-side notifications.
///
/// Private for the same reason as [`render_or_error`]: it does not run
/// `before_render`. Reached only from [`page_with_toast`]'s inline path,
/// which is taken exactly when no Lua participates in the render.
fn html_with_toast(state: &AdminState, template: &str, data: &Value, toast: &str) -> Response {
    match state.render(template, data) {
        Ok(html) => with_toast_header(Html(html).into_response(), toast),
        Err(e) => {
            error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
                .into_response()
        }
    }
}

/// Attach the `X-Crap-Toast` notification header to a response. Shared so
/// the inline and blocking render paths emit an identical header.
fn with_toast_header(mut resp: Response, toast: &str) -> Response {
    let json_toast = json!({ "message": toast, "type": "error" }).to_string();

    if let Ok(val) = json_toast.parse() {
        resp.headers_mut().insert("X-Crap-Toast", val);
    }

    resp
}

/// Return a 422 response with only the toast header — HTMX won't swap the body,
/// so the user keeps their form data while seeing the error notification.
pub fn toast_only_error(msg: &str) -> Response {
    let json_toast = json!({ "message": msg, "type": "error" }).to_string();

    let mut resp = Response::builder()
        .status(StatusCode::UNPROCESSABLE_ENTITY)
        .body(axum::body::Body::empty())
        .unwrap();

    if let Ok(val) = json_toast.parse() {
        resp.headers_mut().insert("X-Crap-Toast", val);
    }

    resp
}

/// Render a template, falling back to a plain error page on failure.
///
/// Private on purpose: it does **not** run `before_render`. Every page goes
/// through [`render_page`] or [`render_auth_page`], which run the hook first
/// — a handler reaching for a raw render would silently opt its page out.
fn render_or_error(state: &AdminState, template: &str, data: &Value) -> Response {
    match state.render(template, data) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            error!("Template render error: {}", e);
            // 500, not 200 (ledger class L8): an infrastructure failure
            // must not read as success to monitors or htmx.
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string()),
            )
                .into_response()
        }
    }
}

/// Render a 400 Bad Request page with the given message. Used when a
/// present-but-invalid query parameter must hard-error instead of being
/// silently ignored (e.g. an unknown list-filter operator).
pub fn bad_request(state: &AdminState, message: &str) -> Response {
    let ctx = ErrorPage {
        base: BasePageContext::for_handler(
            state,
            None,
            None,
            PageMeta::new(PageType::Error400, "bad_request_page_title"),
        ),
        message: message.to_string(),
    };

    let data = to_value(&ctx).expect("ErrorPage serializes infallibly");

    let html = match render_error_page(state, "errors/400", data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>400</h1><p>{}</p>", html_escape(message))),
    };

    (StatusCode::BAD_REQUEST, html).into_response()
}

/// Render a 404 Not Found page with the given message.
pub fn not_found(state: &AdminState, message: &str) -> Response {
    let ctx = ErrorPage {
        base: BasePageContext::for_handler(
            state,
            None,
            None,
            PageMeta::new(PageType::Error404, "not_found_page_title"),
        ),
        message: message.to_string(),
    };

    let data = to_value(&ctx).expect("ErrorPage serializes infallibly");

    let html = match render_error_page(state, "errors/404", data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>404</h1><p>{}</p>", html_escape(message))),
    };

    (StatusCode::NOT_FOUND, html).into_response()
}

/// Look up a collection definition by slug, returning an owned clone or the
/// canonical admin 404 response. The single chokepoint for the
/// "Collection '{slug}' not found" HTML page so the message and status can't
/// drift across the handler surface.
///
/// # Errors
///
/// Returns the rendered 404 [`Response`] (boxed — it is the large variant) when
/// no collection matches `slug`.
pub fn require_collection(
    state: &AdminState,
    slug: &str,
) -> Result<Arc<CollectionDefinition>, Box<Response>> {
    state
        .infra
        .registry
        .get_collection(slug)
        .cloned()
        .ok_or_else(|| Box::new(not_found(state, &format!("Collection '{slug}' not found"))))
}

/// Look up a global definition by slug, returning an owned clone or the
/// canonical admin 404 response. Companion to [`require_collection`].
///
/// # Errors
///
/// Returns the rendered 404 [`Response`] (boxed — it is the large variant) when
/// no global matches `slug`.
pub fn require_global(
    state: &AdminState,
    slug: &str,
) -> Result<Arc<GlobalDefinition>, Box<Response>> {
    state
        .infra
        .registry
        .get_global(slug)
        .cloned()
        .ok_or_else(|| Box::new(not_found(state, &format!("Global '{slug}' not found"))))
}

/// Build a JSON error response `{"error": message}` with the given status.
///
/// The chokepoint for admin XHR/`fetch()` endpoints (back-references, the
/// delete/empty-trash dialog): the HTML `not_found`/`forbidden`/`bad_request`
/// helpers above render error *pages* for navigations, while these return the
/// `{"error": …}` envelope the JS consumers read — so sibling JSON endpoints
/// can't drift on status code or body shape.
pub fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// JSON 404 `{"error": message}`.
pub fn json_not_found(message: &str) -> Response {
    json_error(StatusCode::NOT_FOUND, message)
}

/// JSON 403 `{"error": message}`.
pub fn json_forbidden(message: &str) -> Response {
    json_error(StatusCode::FORBIDDEN, message)
}

/// JSON 400 `{"error": message}`.
pub fn json_bad_request(message: &str) -> Response {
    json_error(StatusCode::BAD_REQUEST, message)
}

/// JSON 409 `{"error": message}` — a precondition conflict, e.g. deleting a
/// document other documents still reference.
pub fn json_conflict(message: &str) -> Response {
    json_error(StatusCode::CONFLICT, message)
}

/// JSON 500 `{"error": message}`.
pub fn json_server_error(message: &str) -> Response {
    json_error(StatusCode::INTERNAL_SERVER_ERROR, message)
}

/// Look up a collection for a JSON/XHR endpoint, returning the owned clone or a
/// boxed JSON 404. The `fetch()` companion to [`require_collection`].
///
/// # Errors
///
/// Returns the boxed JSON 404 [`Response`] when no collection matches `slug`.
pub fn require_collection_json(
    state: &AdminState,
    slug: &str,
) -> Result<Arc<CollectionDefinition>, Box<Response>> {
    state
        .infra
        .registry
        .get_collection(slug)
        .cloned()
        .ok_or_else(|| Box::new(json_not_found(&format!("Collection '{slug}' not found"))))
}

/// Convert a [`ServiceError`] into an admin HTML response.
///
/// `AccessDenied` renders the 403 page with the caller-supplied `denied_msg`
/// (which is shown to the user and so should be friendly + collection-/
/// entity-aware, e.g. `"You don't have permission to view this item"`). All
/// other variants log at `error!` (so the operator can correlate the
/// `Display` text from the underlying error) and render the generic 500
/// page.
///
/// Pairs with [`task_join_error_response`] so the
/// `Result<Result<_, ServiceError>, JoinError>` shape from
/// `tokio::task::spawn_blocking` collapses to two short return arms in the
/// caller — see e.g. `handlers/collections/item/edit_form.rs`.
pub fn service_error_to_admin_response(
    state: &AdminState,
    err: ServiceError,
    denied_msg: &str,
) -> Response {
    match err {
        ServiceError::AccessDenied(_) => forbidden(state, denied_msg),
        e => {
            error!("Service error: {}", e);
            server_error(state, "An internal error occurred.")
        }
    }
}

/// Convert a [`tokio::task::JoinError`] into a generic admin HTML 500
/// response. Tokio task failures generally indicate a panic in the
/// `spawn_blocking` body and are not user-facing. Logged at `error!`.
pub fn task_join_error_response(state: &AdminState, err: &tokio::task::JoinError) -> Response {
    error!("spawn_blocking task error: {}", err);
    server_error(state, "An internal error occurred.")
}

/// Render a 500 Internal Server Error page with the given message.
pub fn server_error(state: &AdminState, message: &str) -> Response {
    let ctx = ErrorPage {
        base: BasePageContext::for_handler(
            state,
            None,
            None,
            PageMeta::new(PageType::Error500, "server_error_page_title"),
        ),
        message: message.to_string(),
    };

    let data = to_value(&ctx).expect("ErrorPage serializes infallibly");

    let html = match render_error_page(state, "errors/500", data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>500</h1><p>{}</p>", html_escape(message))),
    };

    (StatusCode::INTERNAL_SERVER_ERROR, html).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn htmx_redirect_returns_200_with_header() {
        let resp = htmx_redirect("/admin/collections/posts");
        assert_eq!(resp.status(), StatusCode::OK);
        let hx = resp.headers().get("HX-Redirect").unwrap();
        assert_eq!(hx, "/admin/collections/posts");
    }

    #[test]
    fn redirect_response_returns_303() {
        let resp = redirect_response("/admin/collections");
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    }
}
