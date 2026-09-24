//! Whether the viewer may see the upload-collection document that owns a
//! served file — the per-document half of the serve gate.

use std::sync::Arc;

use anyhow::Result as AnyResult;

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document,
        upload::{CollectionUpload, served_url, upload_file_keys},
    },
    db::{DbConnection, DbPool, Filter, FilterClause, FilterOp, FindQuery, LocaleContext, query},
    hooks::HookRunner,
    service::{
        FindDocumentsInput, RunnerReadHooks, ServiceContext, ServiceError, find_documents,
        find_draft_view_stored,
    },
};

/// Owned inputs for [`upload_doc_visible`]'s `spawn_blocking` call.
pub(super) struct UploadVisibilityInput {
    pub pool: DbPool,
    pub runner: HookRunner,
    pub def: Arc<CollectionDefinition>,
    pub slug: String,
    pub filename: String,
    pub user_doc: Option<Document>,
    pub locale_config: LocaleConfig,
}

/// One requested file, resolved against the viewer's read context.
struct OwnerLookup<'a, 'c> {
    ctx: &'a ServiceContext<'c>,
    conn: &'a dyn DbConnection,
    upload: &'a CollectionUpload,
    /// The storage key the request names (`{collection}/{filename}`).
    key: &'a str,
    /// The url the write path stores for that key.
    url: &'a str,
    locale_ctx: Option<&'a LocaleContext>,
}

/// Whether the viewer may see the upload-collection **document** that owns this
/// file, applying the full content-view model — published ∪ draft (downgraded to
/// the viewer's access), with the read/draft hooks' row constraints matched
/// against the row, trashed rows excluded.
///
/// Every served file is owned by exactly one upload-collection document; the doc
/// carries the file's URL in `url` (original) or a `{size}[_fmt]_url` column
/// (variant). We reproduce the *stored* URL from the requested key via
/// `served_url` — the backend-agnostic proxy path the write path stores on every
/// backend — and match it against those columns. (Using the backend's
/// `public_url` here would 404 access-gated uploads on S3/custom, where the
/// direct object/CDN URL differs from the stored proxy path.)
///
/// A file a pending draft replaced its document's file with is named by the
/// draft's version snapshot only, never by a row — so when no row owns the file,
/// the latest drafts are consulted too, through the viewer's draft view.
///
/// An orphan (no owning doc), a non-upload collection, or a refusing or failing
/// read → not visible; a database error is returned, so the viewer is told to
/// retry rather than that the file doesn't exist.
pub(super) fn upload_doc_visible(input: &UploadVisibilityInput) -> AnyResult<bool> {
    let Some(upload) = input.def.upload.as_ref() else {
        return Ok(false);
    };

    let key = format!("{}/{}", input.slug, input.filename);
    let url = served_url(&key);

    let conn = input.pool.get()?;

    let hooks = RunnerReadHooks::new(&input.runner, &conn, input.user_doc.as_ref(), None);
    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(input.user_doc.as_ref())
        .locale_config(Some(&input.locale_config))
        .build();

    // A localized upload collection (e.g. a `caption` field marked `localized`)
    // stores that column per-locale (`caption__en`), so the SELECT needs a locale
    // context — without one it references the bare logical column (`caption`),
    // the query errors, and every file 404s. The default locale is sufficient:
    // the gate only resolves the owning row, not a specific translation.
    let locale_ctx = LocaleContext::default_for(&input.locale_config);

    let lookup = OwnerLookup {
        ctx: &ctx,
        conn: &conn,
        upload,
        key: &key,
        url: &url,
        locale_ctx: locale_ctx.as_ref(),
    };

    if row_owner_visible(&lookup)? {
        return Ok(true);
    }

    if !input.def.has_drafts() {
        return Ok(false);
    }

    draft_owner_visible(&lookup)
}

/// Whether a document row the viewer can see names the file in one of its
/// url-bearing columns.
fn row_owner_visible(lookup: &OwnerLookup<'_, '_>) -> AnyResult<bool> {
    let or_clauses: Vec<FilterClause> = lookup
        .upload
        .url_field_names()
        .into_iter()
        .map(|col| {
            FilterClause::Single(Filter {
                field: col,
                op: FilterOp::Equals(lookup.url.to_string()),
            })
        })
        .collect();

    let fq = FindQuery::builder()
        .filters(vec![FilterClause::or(or_clauses)])
        .limit(Some(1))
        .build();

    // `include_drafts` lets a draft upload serve to a viewer with draft access;
    // the service downgrades to what each viewer may actually see.
    let find_input = FindDocumentsInput::builder(&fq)
        .include_drafts(true)
        .locale_ctx(lookup.locale_ctx)
        .build();

    visible_or_retry(find_documents(lookup.ctx, &find_input), |r| {
        !r.docs.is_empty()
    })
}

/// Whether a document whose latest draft names the file shows that draft to
/// the viewer. The draft is read through the viewer's own draft view — a
/// viewer without draft access, or whose draft rule excludes the document,
/// reads no draft, so a drafted file stays as private as the draft itself.
fn draft_owner_visible(lookup: &OwnerLookup<'_, '_>) -> AnyResult<bool> {
    let slug = lookup.ctx.slug;
    let columns = lookup.upload.url_field_names();

    for parent in query::find_draft_parents_naming(lookup.conn, slug, &columns, lookup.url)? {
        let draft = find_draft_view_stored(lookup.ctx, &parent, lookup.locale_ctx);

        if visible_or_retry(draft, |doc| names_key(doc.as_ref(), lookup))? {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Whether the document read names the requested file in one of its
/// server-derived url columns.
fn names_key(doc: Option<&Document>, lookup: &OwnerLookup<'_, '_>) -> bool {
    doc.is_some_and(|d| {
        upload_file_keys(&d.fields, lookup.upload)
            .iter()
            .any(|k| k == lookup.key)
    })
}

/// Whether a read for the owning document shows it: a transient database error
/// is returned, so the viewer is told to retry; any other failure — a refusing
/// rule, a failing hook — means not visible.
fn visible_or_retry<T>(
    result: Result<T, ServiceError>,
    shows: impl FnOnce(T) -> bool,
) -> AnyResult<bool> {
    match result {
        Ok(found) => Ok(shows(found)),
        Err(ServiceError::Transient(e)) => Err(e),
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    /// Regression: a database error while resolving the owning document answered
    /// 404, telling a signed-in viewer under load that the file doesn't exist.
    #[test]
    fn a_transient_read_error_is_retryable_and_a_refusal_is_not_visible() {
        let transient: Result<(), ServiceError> = Err(ServiceError::Transient(anyhow!(
            "timed out waiting for connection"
        )));
        assert!(visible_or_retry(transient, |()| true).is_err());

        let refused: Result<(), ServiceError> = Err(ServiceError::AccessDenied("no".into()));
        assert!(!visible_or_retry(refused, |()| true).unwrap());

        assert!(visible_or_retry(Ok(()), |()| true).unwrap());
    }
}
