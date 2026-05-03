//! `GET /admin/p/{slug}` — render a filesystem-routed custom admin page.
//!
//! The page's template lives at `<config_dir>/templates/pages/<slug>.hbs`
//! (registered automatically by the Handlebars overlay loader). Page
//! existence is determined by template registration; sidebar nav and
//! permission metadata come from the `CustomPageRegistry` populated by
//! Lua's `crap.pages.register`.

use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};

use crate::{
    admin::{
        AdminState,
        context::{BasePageContext, PageMeta, PageType, page::custom::CustomPage},
        custom_pages::is_valid_slug,
        handlers::shared::{
            extract_editor_locale, forbidden, get_user_doc, has_read_access, not_found, render_page,
        },
    },
    core::auth::{AuthUser, Claims},
};

/// GET /admin/p/{slug} — render a custom admin page.
pub async fn render_custom_page(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    if !is_valid_slug(&slug) {
        return not_found(&state, &format!("Page '{}' not found", slug));
    }

    let template_name = format!("pages/{}", slug);
    if state.handlebars.get_template(&template_name).is_none() {
        return not_found(&state, &format!("Page '{}' not found", slug));
    }

    // Enforce per-page access — same pattern as `access.read` on
    // collections. When the page declares `access = "fn_name"` and the
    // function returns false (or denies), respond 403.
    let registered = state.custom_pages.get(&slug);
    if let Some(access_ref) = registered.and_then(|p| p.access.as_deref()) {
        let user_doc = get_user_doc(&auth_user);
        if !has_read_access(&state, Some(access_ref), user_doc) {
            return forbidden(&state, "You don't have permission to view this page");
        }
    }

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    // Use the registered label as the page title when present; fall back
    // to the slug so the layout's `<title>` is at least non-empty.
    let title = registered
        .and_then(|p| p.label.clone())
        .unwrap_or_else(|| slug.clone());

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        &auth_user,
        PageMeta::new(PageType::CustomPage, title),
    )
    .with_editor_locale(editor_locale.as_deref(), &state);

    let ctx = CustomPage { base, slug };

    render_page(&state, &template_name, &ctx)
}
