use crate::admin::AdminState;
use crate::core::auth::AuthUser;
use crate::db::query::{self, AccessResult};
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Extension,
};

use crate::admin::handlers::shared::{
    check_access_or_forbid, forbidden, htmx_redirect, redirect_response,
};

/// POST /admin/collections/{slug}/{id}/versions/{version_id}/restore — restore a version
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, id, version_id)): Path<(String, String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections"),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/collections/{}/{}", slug, id));
    }

    // Check update access
    match check_access_or_forbid(
        &state,
        def.access.update.as_deref(),
        &auth_user,
        Some(&id),
        None,
    ) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to update this item")
                .into_response()
        }
        Err(resp) => return resp,
        _ => {}
    }

    let pool = state.pool.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let def_owned = def.clone();
    let locale_config = state.config.locale.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut conn = pool
            .get()
            .map_err(|e| anyhow::anyhow!("DB connection: {}", e))?;
        let tx = conn
            .transaction()
            .map_err(|e| anyhow::anyhow!("Start transaction: {}", e))?;
        let version = query::find_version_by_id(&tx, &slug_owned, &version_id)?
            .ok_or_else(|| anyhow::anyhow!("Version not found"))?;
        let doc = query::restore_version(
            &tx,
            &slug_owned,
            &def_owned,
            &id_owned,
            &version.snapshot,
            "published",
            &locale_config,
        )?;
        tx.commit().map_err(|e| anyhow::anyhow!("Commit: {}", e))?;
        Ok::<_, anyhow::Error>(doc)
    })
    .await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&format!("/admin/collections/{}/{}", slug, id)),
        Ok(Err(e)) => {
            tracing::error!("Restore version error: {}", e);
            htmx_redirect(&format!("/admin/collections/{}/{}", slug, id))
        }
        Err(e) => {
            tracing::error!("Restore version task error: {}", e);
            htmx_redirect(&format!("/admin/collections/{}/{}", slug, id))
        }
    }
}
