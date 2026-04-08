//! Service-layer read operations for collections and globals.
//!
//! Centralizes the read lifecycle (hooks → query → hydrate → populate → strip)
//! shared across admin, gRPC, MCP, and Lua CRUD surfaces.

use crate::{
    core::{
        CollectionDefinition, Document, Registry, cache::CacheBackend,
        collection::GlobalDefinition, upload,
    },
    db::{AccessResult, DbConnection, FilterClause, FindQuery, LocaleContext, ops, query},
    hooks::lifecycle::AfterReadCtx,
};

use super::{ServiceError, read_hooks::ReadHooks};

type Result<T> = std::result::Result<T, ServiceError>;

/// Options controlling read behavior and post-processing.
pub struct ReadOptions<'a> {
    /// Relationship population depth (0 = skip).
    pub depth: i32,
    /// Whether to hydrate join-table data (arrays, blocks, has-many).
    /// Ignored by `find_document_by_id` which uses `ops::find_by_id_full` (handles its own hydration).
    pub hydrate: bool,
    /// Optional field selection filter.
    pub select: Option<&'a [String]>,
    /// Locale context for localized queries.
    pub locale_ctx: Option<&'a LocaleContext>,
    /// Registry for relationship population.
    pub registry: Option<&'a Registry>,
    /// Authenticated user (for field-level access + hook context).
    pub user: Option<&'a Document>,
    /// UI locale (for hook context).
    pub ui_locale: Option<&'a str>,
    /// Whether to overlay draft version data (find_by_id only).
    pub use_draft: bool,
    /// Access constraint filters for find_by_id (pre-computed by caller).
    pub access_constraints: Option<Vec<FilterClause>>,
    /// Optional cache backend for relationship population.
    pub cache: Option<&'a dyn CacheBackend>,
}

impl Default for ReadOptions<'_> {
    fn default() -> Self {
        Self {
            depth: 0,
            hydrate: true,
            select: None,
            locale_ctx: None,
            registry: None,
            user: None,
            ui_locale: None,
            use_draft: false,
            access_constraints: None,
            cache: None,
        }
    }
}

/// Result of a find operation.
pub struct FindResult {
    pub docs: Vec<Document>,
    pub total: i64,
}

/// Execute a paginated find query with the full read lifecycle.
///
/// Steps: before_read → find + count → hydrate → populate → upload sizes →
/// select strip → field-level read strip → after_read.
///
/// Access control (constraint filters) must be pre-applied by the caller
/// into `find_query.filters`.
pub fn find_documents(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    slug: &str,
    def: &CollectionDefinition,
    find_query: &FindQuery,
    opts: &ReadOptions,
) -> Result<FindResult> {
    // Collection-level access check — short-circuit if denied
    let access = hooks.check_access(def.access.read.as_deref(), opts.user, None, None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    // Merge access constraints into filters
    let mut fq = find_query.clone();
    if let AccessResult::Constrained(extra) = access {
        fq.filters.extend(extra);
    }

    hooks.before_read(&def.hooks, slug, "find")?;

    let mut docs = query::find(conn, slug, def, &fq, opts.locale_ctx)?;
    let total = query::count_with_search(
        conn,
        slug,
        def,
        &fq.filters,
        opts.locale_ctx,
        fq.search.as_deref(),
        fq.include_deleted,
    )?;

    post_process_docs(conn, hooks, slug, def, &mut docs, opts, "find");

    Ok(FindResult { docs, total })
}

/// Count documents matching the given filters, with access control.
///
/// Applies collection-level access check and merges any constraint filters.
#[allow(clippy::too_many_arguments)]
pub fn count_documents(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    slug: &str,
    def: &CollectionDefinition,
    filters: &[FilterClause],
    locale_ctx: Option<&LocaleContext>,
    search: Option<&str>,
    include_deleted: bool,
    user: Option<&Document>,
) -> Result<i64> {
    let access = hooks.check_access(def.access.read.as_deref(), user, None, None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    let mut merged = filters.to_vec();
    if let AccessResult::Constrained(extra) = access {
        merged.extend(extra);
    }

    let count = query::count_with_search(
        conn,
        slug,
        def,
        &merged,
        locale_ctx,
        search,
        include_deleted,
    )?;

    Ok(count)
}

/// Look up a single document by ID with the full read lifecycle.
///
/// Steps: before_read → find_by_id → hydrate → populate → upload sizes →
/// select strip → field-level read strip → after_read.
///
/// Returns `None` if the document doesn't exist (or is filtered by access constraints).
pub fn find_document_by_id(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    opts: &ReadOptions,
) -> Result<Option<Document>> {
    // Collection-level access check — short-circuit if denied
    let access = hooks.check_access(def.access.read.as_deref(), opts.user, Some(id), None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    // Merge caller-provided + access-derived constraints
    let constraints = match (opts.access_constraints.clone(), access) {
        (Some(mut existing), AccessResult::Constrained(extra)) => {
            existing.extend(extra);
            Some(existing)
        }
        (Some(existing), _) => Some(existing),
        (None, AccessResult::Constrained(extra)) => Some(extra),
        _ => None,
    };

    hooks.before_read(&def.hooks, slug, "find_by_id")?;

    let mut doc = match ops::find_by_id_full(
        conn,
        slug,
        def,
        id,
        opts.locale_ctx,
        constraints,
        opts.use_draft,
    )? {
        Some(d) => d,
        None => return Ok(None),
    };

    // Post-process (skip hydration — find_by_id_full already handled it)
    post_process_single(conn, hooks, slug, def, &mut doc, opts, "find_by_id");

    Ok(Some(doc))
}

/// Read a global document with the full read lifecycle.
///
/// Steps: before_read → get_global → field-level read strip → after_read.
pub fn get_global_document(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    slug: &str,
    def: &GlobalDefinition,
    locale_ctx: Option<&LocaleContext>,
    user: Option<&Document>,
    ui_locale: Option<&str>,
) -> Result<Document> {
    let access = hooks.check_access(def.access.read.as_deref(), user, None, None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    hooks.before_read(&def.hooks, slug, "get")?;

    let mut doc = query::get_global(conn, slug, def, locale_ctx)?;

    let denied = hooks.field_read_denied(&def.fields, user);
    for name in &denied {
        doc.fields.remove(name);
    }

    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: slug,
        operation: "get",
        user,
        ui_locale,
    };

    Ok(hooks.after_read_one(&ar_ctx, doc))
}

/// Post-process a single document (skip hydration — used by find_by_id where
/// `ops::find_by_id_full` already handled hydration).
fn post_process_single(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    slug: &str,
    def: &CollectionDefinition,
    doc: &mut Document,
    opts: &ReadOptions,
    operation: &str,
) {
    // Populate relationships
    if opts.depth > 0
        && let Some(registry) = opts.registry
    {
        let mut visited = std::collections::HashSet::new();
        let pop_ctx = query::PopulateContext::new(conn, registry, slug, def);
        let mut pop_opts = query::PopulateOpts::new(opts.depth);
        if let Some(s) = opts.select {
            pop_opts = pop_opts.select(s);
        }
        if let Some(lc) = opts.locale_ctx {
            pop_opts = pop_opts.locale_ctx(lc);
        }
        let pop_result = if let Some(cache) = opts.cache {
            query::populate_relationships_cached(&pop_ctx, doc, &mut visited, &pop_opts, cache)
        } else {
            query::populate_relationships(&pop_ctx, doc, &mut visited, &pop_opts)
        };
        if let Err(e) = pop_result {
            tracing::warn!("populate error for {slug}/{}: {e:#}", doc.id);
        }
    }

    if let Some(ref upload_config) = def.upload
        && upload_config.enabled
    {
        upload::assemble_sizes_object(doc, upload_config);
    }

    if let Some(sel) = opts.select {
        query::apply_select_to_document(doc, sel);
    }

    let denied = hooks.field_read_denied(&def.fields, opts.user);
    for name in &denied {
        doc.fields.remove(name);
    }

    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: slug,
        operation,
        user: opts.user,
        ui_locale: opts.ui_locale,
    };
    // Swap in a placeholder, run hooks, swap back
    let placeholder = Document::new("".to_string());
    let owned = std::mem::replace(doc, placeholder);
    *doc = hooks.after_read_one(&ar_ctx, owned);
}

/// Shared post-processing for find: hydrate, populate, upload sizes,
/// select stripping, field-level access stripping, and after_read hooks.
fn post_process_docs(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    slug: &str,
    def: &CollectionDefinition,
    docs: &mut Vec<Document>,
    opts: &ReadOptions,
    operation: &str,
) {
    if opts.hydrate {
        for doc in docs.iter_mut() {
            if let Err(e) =
                query::hydrate_document(conn, slug, &def.fields, doc, opts.select, opts.locale_ctx)
            {
                tracing::warn!("hydrate error for {slug}/{}: {e:#}", doc.id);
            }
        }
    }

    if opts.depth > 0
        && let Some(registry) = opts.registry
    {
        let pop_ctx = query::PopulateContext::new(conn, registry, slug, def);
        let mut pop_opts = query::PopulateOpts::new(opts.depth);
        if let Some(s) = opts.select {
            pop_opts = pop_opts.select(s);
        }
        if let Some(lc) = opts.locale_ctx {
            pop_opts = pop_opts.locale_ctx(lc);
        }
        let pop_result = if let Some(cache) = opts.cache {
            query::populate_relationships_batch_cached(&pop_ctx, docs, &pop_opts, cache)
        } else {
            query::populate_relationships_batch(&pop_ctx, docs, &pop_opts)
        };
        if let Err(e) = pop_result {
            tracing::warn!("populate error for {slug}: {e:#}");
        }
    }

    if let Some(ref upload_config) = def.upload
        && upload_config.enabled
    {
        for doc in docs.iter_mut() {
            upload::assemble_sizes_object(doc, upload_config);
        }
    }

    if let Some(sel) = opts.select {
        for doc in docs.iter_mut() {
            query::apply_select_to_document(doc, sel);
        }
    }

    let denied = hooks.field_read_denied(&def.fields, opts.user);
    if !denied.is_empty() {
        for doc in docs.iter_mut() {
            for name in &denied {
                doc.fields.remove(name);
            }
        }
    }

    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: slug,
        operation,
        user: opts.user,
        ui_locale: opts.ui_locale,
    };
    let processed = hooks.after_read_many(&ar_ctx, std::mem::take(docs));
    *docs = processed;
}
