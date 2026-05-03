use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::json;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, GlobalContext, PageMeta, PageType,
            page::globals::GlobalRestoreConfirmPage,
        },
        handlers::shared::{
            check_access_or_forbid, extract_editor_locale, forbidden,
            load_version_with_missing_relations, not_found, paths, redirect_response, render_page,
            server_error,
        },
    },
    core::auth::{AuthUser, Claims},
    db::query::AccessResult,
    service,
};

/// GET /admin/globals/{slug}/versions/{version_id}/restore — confirmation page
pub async fn restore_confirm(
    State(state): State<AdminState>,
    Path((slug, version_id)): Path<(String, String)>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Global '{}' not found", slug)),
    };

    if !def.has_versions() {
        return redirect_response(&paths::global(&slug));
    }

    match check_access_or_forbid(&state, def.access.update.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to update this global");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return server_error(&state, "Database error"),
    };

    let version_ctx = service::ServiceContext::global(&slug, &def)
        .conn(&conn)
        .build();

    let (version, missing) = match load_version_with_missing_relations(
        &version_ctx,
        &conn,
        &state.registry,
        &version_id,
        &def.fields,
    ) {
        Ok(data) => data,
        Err(msg) => return server_error(&state, msg),
    };

    let restore_url = paths::global_version_restore(&slug, &version_id);
    let back_url = paths::global(&slug);

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let breadcrumbs = vec![
        Breadcrumb::link("dashboard", "/admin"),
        Breadcrumb::link(def.display_name(), paths::global(&slug)),
        Breadcrumb::current("restore_version"),
    ];

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        &auth_user,
        PageMeta::new(PageType::GlobalVersions, "restore_version"),
    )
    .with_editor_locale(editor_locale.as_deref(), &state)
    .with_breadcrumbs(breadcrumbs);

    let ctx = GlobalRestoreConfirmPage {
        base,
        global: GlobalContext::from_def(&def),
        version_number: json!(version.version),
        missing_relations: missing.into_iter().map(|m| json!(m)).collect(),
        restore_url,
        back_url,
    };

    render_page(&state, "globals/restore", &ctx)
}
