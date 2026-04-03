use anyhow::{Error, anyhow};
use axum::{
    Extension,
    extract::{Path, State},
    response::Response,
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{check_access_or_forbid, forbidden, htmx_redirect, redirect_response},
    },
    config::LocaleConfig,
    core::{Document, auth::AuthUser, collection::GlobalDefinition},
    db::{
        DbPool,
        query::{self, AccessResult},
    },
};

/// Find the version snapshot and restore it inside a transaction.
fn restore_from_version(
    pool: &DbPool,
    slug: &str,
    def: &GlobalDefinition,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document, Error> {
    let global_table = format!("_global_{}", slug);

    let mut conn = pool.get().map_err(|e| anyhow!("DB connection: {}", e))?;
    let tx = conn
        .transaction()
        .map_err(|e| anyhow!("Start transaction: {}", e))?;

    let version = query::find_version_by_id(&tx, &global_table, version_id)?
        .ok_or_else(|| anyhow!("Version not found"))?;

    let doc = query::restore_global_version(
        &tx,
        slug,
        def,
        &version.snapshot,
        "published",
        locale_config,
    )?;

    tx.commit().map_err(|e| anyhow!("Commit: {}", e))?;

    Ok(doc)
}

/// POST /admin/globals/{slug}/versions/{version_id}/restore
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, version_id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin"),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/globals/{}", slug));
    }

    match check_access_or_forbid(&state, def.access.update.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to update this global");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let redirect = format!("/admin/globals/{}", slug);
    let pool = state.pool.clone();
    let def_owned = def.clone();
    let locale_config = state.config.locale.clone();

    let result = task::spawn_blocking(move || {
        restore_from_version(&pool, &slug, &def_owned, &version_id, &locale_config)
    })
    .await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&redirect),
        Ok(Err(e)) => {
            error!("Restore global version error: {}", e);
            htmx_redirect(&redirect)
        }
        Err(e) => {
            error!("Restore global version task error: {}", e);
            htmx_redirect(&redirect)
        }
    }
}
