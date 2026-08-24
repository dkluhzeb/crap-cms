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
            check_access_or_forbid, extract_editor_locale, forbidden, global_base,
            load_version_with_missing_relations, paths, redirect_response, render_page,
            require_global, server_error,
        },
    },
    core::auth::{AuthUser, Claims},
    db::query::AccessResult,
    service::{self, RunnerReadHooks},
};

/// GET /`admin/globals/{slug}/versions/{version_id}/restore` — confirmation page
pub async fn restore_confirm(
    State(state): State<AdminState>,
    Path((slug, version_id)): Path<(String, String)>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match require_global(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    if !def.has_versions() {
        return redirect_response(&paths::global(&slug));
    }

    match check_access_or_forbid(
        &state,
        def.access.update.as_ref(),
        auth_user.as_ref(),
        None,
        None,
        "update",
        &slug,
    ) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to update this global");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let Ok(conn) = state.pool.get() else {
        return server_error(&state, "Database error");
    };

    // `find_version_by_id` (called by `load_version_with_missing_relations`)
    // runs an access check that requires `ServiceContext.read_hooks`.
    let user_doc = auth_user.as_ref().map(|Extension(u)| &u.user_doc);
    let read_hooks = RunnerReadHooks::new(&state.hook_runner, &conn, user_doc, None);
    let version_ctx = service::ServiceContext::global(&slug, &def)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
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

    let mut breadcrumbs = global_base(&def, &slug);
    breadcrumbs.push(Breadcrumb::current("restore_version"));

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        auth_user.as_ref(),
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
