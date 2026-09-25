use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::json;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, GlobalContext, PageMeta, PageType,
            page::globals::GlobalRestoreConfirmPage,
        },
        handlers::shared::{
            HxNav, PageRequest, check_access_or_forbid, extract_editor_locale, forbidden,
            global_base, load_version_with_restore_gaps, paths, redirect_response, render_page,
            require_global, server_error,
        },
    },
    core::{
        Document, GlobalDefinition,
        auth::{AuthUser, Claims},
        document::VersionSnapshot,
    },
    service::{self, RunnerReadHooks, VersionGaps, global_access_allowed},
};

/// Load the global version being restored plus what it can no longer resolve.
///
/// Scoped to its own function so the pooled connection and the read hooks
/// borrowing it — neither of which is `Send` — are dropped before the
/// handler awaits the page render.
///
/// A global is never an upload collection, so the file half of the report is
/// always empty here; the relation half is the whole check.
fn load_restore_data(
    state: &AdminState,
    slug: &str,
    def: &GlobalDefinition,
    version_id: &str,
    user_doc: Option<&Document>,
) -> Result<(VersionSnapshot, VersionGaps), &'static str> {
    let Ok(conn) = state.infra.pool.get() else {
        return Err("Database error");
    };

    // The version read runs an access check that requires
    // `ServiceContext.read_hooks`. The locale config lets the missing-relations
    // check read every locale the version records.
    let read_hooks = RunnerReadHooks::new(&state.infra.hook_runner, &conn, user_doc, None);
    let ctx = service::ServiceContext::global(slug, def)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
        .locale_config(Some(&state.infra.locale_config))
        .build();

    load_version_with_restore_gaps(&ctx, &state.infra.registry, version_id)
}

/// Gate the confirmation page on the global's `update` access, mapped as every
/// global surface maps it: a filter table is a configuration error (the
/// restore itself refuses it), never an allow.
fn check_restore_access(
    state: &AdminState,
    def: &GlobalDefinition,
    auth_user: Option<&Extension<AuthUser>>,
    slug: &str,
) -> Result<(), Box<Response>> {
    let access = check_access_or_forbid(
        state,
        def.access.update.as_ref(),
        auth_user,
        None,
        None,
        "update",
        slug,
    )?;

    match global_access_allowed(&access, slug) {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(forbidden(
            state,
            "You don't have permission to update this global",
        ))),
        Err(e) => {
            error!("Global restore confirm for '{slug}': {e}");

            Err(Box::new(server_error(state, "Access configuration error")))
        }
    }
}

/// GET /`admin/globals/{slug}/versions/{version_id}/restore` — confirmation page
pub async fn restore_confirm(
    State(state): State<AdminState>,
    hx: HxNav,
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

    if let Err(resp) = check_restore_access(&state, &def, auth_user.as_ref(), &slug) {
        return *resp;
    }

    let user_doc = auth_user.as_ref().map(|Extension(u)| &u.user_doc);

    let (version, gaps) = match load_restore_data(&state, &slug, &def, &version_id, user_doc) {
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
        missing_relations: gaps.relations.into_iter().map(|m| json!(m)).collect(),
        missing_files: gaps.files,
        restore_url,
        back_url,
    };

    render_page(
        &state,
        PageRequest::new(hx, auth_user.as_ref()),
        "globals/restore",
        &ctx,
    )
    .await
}
