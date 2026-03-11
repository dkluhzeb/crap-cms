use crate::admin::AdminState;
use crate::core::auth::AuthUser;
use crate::db::query;
use crate::db::query::AccessResult;
use axum::{
    Extension,
    extract::{Path, State},
    response::IntoResponse,
};

use crate::admin::handlers::shared::{
    check_access_or_forbid, forbidden, htmx_redirect, redirect_response,
};

/// POST /admin/globals/{slug}/versions/{version_id}/restore
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, version_id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin"),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/globals/{}", slug));
    }

    // Check update access
    match check_access_or_forbid(&state, def.access.update.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to update this global")
                .into_response();
        }
        Err(resp) => return resp,
        _ => {}
    }

    let pool = state.pool.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let locale_config = state.config.locale.clone();
    let result = tokio::task::spawn_blocking(move || {
        let global_table = format!("_global_{}", slug_owned);
        let mut conn = pool
            .get()
            .map_err(|e| anyhow::anyhow!("DB connection: {}", e))?;
        let tx = conn
            .transaction()
            .map_err(|e| anyhow::anyhow!("Start transaction: {}", e))?;
        let version = query::find_version_by_id(&tx, &global_table, &version_id)?
            .ok_or_else(|| anyhow::anyhow!("Version not found"))?;
        let doc = query::restore_global_version(
            &tx,
            &slug_owned,
            &def_owned,
            &version.snapshot,
            "published",
            &locale_config,
        )?;
        tx.commit().map_err(|e| anyhow::anyhow!("Commit: {}", e))?;
        Ok::<_, anyhow::Error>(doc)
    })
    .await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&format!("/admin/globals/{}", slug)),
        Ok(Err(e)) => {
            tracing::error!("Restore global version error: {}", e);
            htmx_redirect(&format!("/admin/globals/{}", slug))
        }
        Err(e) => {
            tracing::error!("Restore global version task error: {}", e);
            htmx_redirect(&format!("/admin/globals/{}", slug))
        }
    }
}
