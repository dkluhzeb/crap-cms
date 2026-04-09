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
    core::{CollectionDefinition, Document, auth::AuthUser},
    db::DbPool,
    service::restore_collection_version,
};

/// Find the version snapshot and restore it inside a transaction.
fn restore_from_version(
    pool: &DbPool,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    version_id: &str,
    locale_config: &crate::config::LocaleConfig,
) -> anyhow::Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction().context("Start transaction")?;

    let doc = restore_collection_version(&tx, slug, def, id, version_id, locale_config)
        .map_err(|e| e.into_anyhow())?;

    tx.commit().context("Commit")?;
    Ok(doc)
}

/// POST /admin/collections/{slug}/{id}/versions/{version_id}/restore — restore a version
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, id, version_id)): Path<(String, String, String)>,
    _auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections"),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/collections/{}/{}", slug, id));
    }

    let redirect = format!("/admin/collections/{}/{}", slug, id);
    let pool = state.pool.clone();
    let def_owned = def.clone();
    let locale_config = state.config.locale.clone();

    let result = task::spawn_blocking(move || {
        restore_from_version(&pool, &slug, &def_owned, &id, &version_id, &locale_config)
    })
    .await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&redirect),
        Ok(Err(e)) => {
            error!("Restore version error: {}", e);

            htmx_redirect(&redirect)
        }
        Err(e) => {
            error!("Restore version task error: {}", e);

            htmx_redirect(&redirect)
        }
    }
}
