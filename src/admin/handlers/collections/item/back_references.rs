use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use serde_json::json;
use tokio::task;
use tracing::error;

use crate::{
    admin::{AdminState, handlers::shared::check_access_or_forbid},
    config::LocaleConfig,
    core::{Registry, auth::AuthUser},
    db::{DbPool, query::AccessResult},
    hooks::HookRunner,
    service::{
        RunnerReadHooks, ServiceContext, ServiceError,
        document_info::{BackReferenceReport, find_back_references},
    },
};

/// Owned inputs for the blocking back-reference scan (must be `'static`).
struct BackRefParams {
    pool: DbPool,
    runner: HookRunner,
    registry: Arc<Registry>,
    locale: LocaleConfig,
    slug: String,
    target_id: String,
    user_doc: Option<crate::core::Document>,
}

/// Run the access-filtered back-reference scan on a blocking thread.
///
/// Resolves access through the same read hooks as a normal find, so referrers
/// the viewer cannot see are excluded from the report.
fn load_back_references_blocking(
    params: &BackRefParams,
) -> Result<BackReferenceReport, ServiceError> {
    let conn = params.pool.get().map_err(ServiceError::Internal)?;

    let hooks = RunnerReadHooks::new(&params.runner, &conn, params.user_doc.as_ref(), None);
    let ctx = ServiceContext::slug_only(&params.slug)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(params.user_doc.as_ref())
        .build();

    find_back_references(&ctx, &params.registry, &params.target_id, &params.locale)
}

/// GET /admin/collections/{slug}/{id}/back-references — lazy-load the
/// access-filtered back-references shown on the delete page.
pub async fn back_references(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let Some(def) = state.registry.get_collection(&slug).cloned() else {
        return Json(json!({ "error": "Collection not found" })).into_response();
    };

    match check_access_or_forbid(
        &state,
        def.access.read.as_ref(),
        auth_user.as_ref(),
        Some(&id),
        None,
        "read",
        &slug,
    ) {
        Ok(AccessResult::Denied) | Err(_) => {
            return Json(json!({ "error": "Access denied" })).into_response();
        }
        _ => {}
    }

    let params = BackRefParams {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        registry: state.registry.clone(),
        locale: state.config.locale.clone(),
        slug,
        target_id: id,
        user_doc: auth_user.map(|Extension(au)| au.user_doc.clone()),
    };

    match task::spawn_blocking(move || load_back_references_blocking(&params)).await {
        Ok(Ok(report)) => Json(json!(report)).into_response(),
        Ok(Err(e)) => {
            error!("Back-reference scan error: {e}");
            Json(json!({ "error": "Back-reference scan failed" })).into_response()
        }
        Err(e) => {
            error!("Back-reference task join error: {e}");
            Json(json!({ "error": "Back-reference scan failed" })).into_response()
        }
    }
}
