//! Shared post-processing for read operations (populate, upload sizes,
//! select stripping, field-level access, after_read hooks).

use std::collections::HashSet;

use crate::{
    core::{CollectionDefinition, Document, upload},
    db::{DbConnection, query},
    hooks::lifecycle::AfterReadCtx,
    service::hooks::ReadHooks,
};

use super::ReadOptions;

/// Post-process a single document (skip hydration -- used by find_by_id where
/// `ops::find_by_id_full` already handled hydration).
pub(super) fn post_process_single(
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
        let mut visited = HashSet::new();
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
pub(super) fn post_process_docs(
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
