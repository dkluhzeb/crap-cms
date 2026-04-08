use anyhow::Context as _;
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
        handlers::shared::{htmx_redirect, redirect_response},
    },
    core::{Document, auth::AuthUser, collection::GlobalDefinition},
    db::DbPool,
};

/// Find the version snapshot and restore it inside a transaction.
fn restore_from_version(
    pool: &DbPool,
    slug: &str,
    def: &GlobalDefinition,
    version_id: &str,
    locale_config: &crate::config::LocaleConfig,
) -> anyhow::Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction().context("Start transaction")?;

    let doc = crate::service::version_ops::restore_global_version(
        &tx,
        slug,
        def,
        version_id,
        locale_config,
    )
    .map_err(|e| e.into_anyhow())?;

    tx.commit().context("Commit")?;
    Ok(doc)
}

/// POST /admin/globals/{slug}/versions/{version_id}/restore
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, version_id)): Path<(String, String)>,
    _auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin"),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/globals/{}", slug));
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
