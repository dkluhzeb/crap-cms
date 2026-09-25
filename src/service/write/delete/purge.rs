//! The hard-delete purge every path shares, and the cancelling of an upload's
//! queued image conversions.

use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, upload::delete_image_jobs_for_document},
    db::{DbConnection, query},
    service::ServiceError,
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Permanently delete a document's row with every cleanup a hard delete needs:
/// its outgoing reference counts, the row, its full-text entry and its queued
/// image conversions. The one path the service hard delete, the CLI trash purge
/// and the scheduled retention purge share. Returns whether a row was deleted.
///
/// # Errors
///
/// Returns a backend error if the reference-count update, the DELETE or the
/// full-text removal fails.
pub(crate) fn purge_document(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    id: &str,
    locale_config: &LocaleConfig,
) -> Result<bool> {
    let slug = &def.slug;

    query::ref_count::before_hard_delete(conn, slug, id, &def.fields, locale_config)?;

    if !query::delete(conn, slug, id)? {
        return Ok(false);
    }

    if conn.supports_fts() {
        query::fts::fts_delete(conn, slug, id)?;
    }

    cancel_image_jobs(conn, slug, def, id);

    Ok(true)
}

/// Cancel an upload document's queued image conversions, so none runs against
/// a row that is gone — or against a file it has since replaced. Best-effort,
/// logged. Shared by the hard delete and the upload file-replace write.
pub(crate) fn cancel_image_jobs(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
) {
    if def.is_upload_collection() {
        let _ = delete_image_jobs_for_document(conn, slug, id)
            .inspect_err(|e| warn!("Failed to cancel image jobs for {slug}/{id}: {e}"));
    }
}
