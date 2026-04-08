//! Lightweight search for relationship fields.

use crate::{
    core::{CollectionDefinition, Document, upload},
    db::{AccessResult, DbConnection, FindQuery, LocaleContext, query},
    service::{ServiceError, hooks::ReadHooks},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Options for a lightweight search query.
pub struct SearchOptions<'a> {
    pub search: Option<&'a str>,
    pub limit: i64,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub user: Option<&'a Document>,
}

/// Lightweight search for relationship fields -- access check + find + upload sizes.
///
/// Unlike `find_documents`, this skips hooks, hydration, population, and field stripping.
/// Used by the admin relationship search API.
pub fn search_documents(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    slug: &str,
    def: &CollectionDefinition,
    opts: &SearchOptions<'_>,
) -> Result<Vec<Document>> {
    let access = hooks.check_access(def.access.read.as_deref(), opts.user, None, None)?;
    if matches!(access, AccessResult::Denied) {
        return Ok(Vec::new());
    }

    let mut fq = FindQuery::new();
    fq.limit = Some(opts.limit);
    fq.search = opts.search.map(|s| s.to_string());

    if let AccessResult::Constrained(extra) = access {
        fq.filters.extend(extra);
    }

    let mut docs = query::find(conn, slug, def, &fq, opts.locale_ctx)?;

    if let Some(ref uc) = def.upload
        && uc.enabled
    {
        for doc in &mut docs {
            upload::assemble_sizes_object(doc, uc);
        }
    }

    Ok(docs)
}
