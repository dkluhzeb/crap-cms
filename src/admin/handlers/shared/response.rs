//! Response helpers — error pages, redirects, HTMX-aware responses, toast rendering.

use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
};
use serde_json::{Value, json};
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
    },
    core::richtext::renderer::html_escape,
};

/// Render a 403 Forbidden page with the given message.
pub fn forbidden(state: &AdminState, message: &str) -> Response {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error403, "forbidden_page_title")
        .set("message", Value::String(message.to_string()))
        .build();

    let data = state.hook_runner.run_before_render(data);

    let html = match state.render("errors/403", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!(
            "<h1>403 Forbidden</h1><p>{}</p>",
            html_escape(message)
        )),
    };

    (StatusCode::FORBIDDEN, html).into_response()
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

/// Percent-encode a string so it is safe for HTTP header values.
/// Non-ASCII bytes and control characters are encoded as `%XX`.
fn percent_encode_header(s: &str) -> String {
    let mut out = String::with_capacity(s.len());

    for b in s.bytes() {
        if b.is_ascii_graphic() || b == b' ' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }

    out
}

/// Render a template and set the X-Crap-Toast header for client-side notifications.
pub fn html_with_toast(state: &AdminState, template: &str, data: &Value, toast: &str) -> Response {
    match state.render(template, data) {
        Ok(html) => {
            let mut resp = Html(html).into_response();
            let json_toast = json!({ "message": toast, "type": "error" }).to_string();

            if let Ok(val) = json_toast.parse() {
                resp.headers_mut().insert("X-Crap-Toast", val);
            }

            resp
        }
        Err(e) => {
            error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
                .into_response()
        }
    }
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
pub fn render_or_error(state: &AdminState, template: &str, data: &Value) -> Response {
    match state.render(template, data) {
        Ok(html) => Html(html),
        Err(e) => {
            error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
    .into_response()
}

/// Render a 404 Not Found page with the given message.
pub fn not_found(state: &AdminState, message: &str) -> Response {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error404, "not_found_page_title")
        .set("message", Value::String(message.to_string()))
        .build();

    let data = state.hook_runner.run_before_render(data);

    let html = match state.render("errors/404", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>404</h1><p>{}</p>", html_escape(message))),
    };

    (StatusCode::NOT_FOUND, html).into_response()
}

/// Render a 500 Internal Server Error page with the given message.
pub fn server_error(state: &AdminState, message: &str) -> Response {
    let data = ContextBuilder::new(state, None)
        .page(PageType::Error500, "server_error_page_title")
        .set("message", Value::String(message.to_string()))
        .build();

    let data = state.hook_runner.run_before_render(data);

    let html = match state.render("errors/500", &data) {
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
