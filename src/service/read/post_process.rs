//! Shared post-processing for read operations (populate, upload sizes,
//! select stripping, field-level access, after_read hooks).

use std::{collections::HashSet, mem};

use tracing::warn;

use crate::{
    core::{Document, Registry, cache::CacheBackend, upload},
    db::{DbConnection, LocaleContext, query},
    hooks::lifecycle::AfterReadCtx,
    service::{ServiceContext, helpers},
};

/// Fields needed by post-processing. Implemented by all read input structs.
pub(crate) trait PostProcessOpts {
    fn depth(&self) -> i32;
    fn hydrate(&self) -> bool;
    fn select(&self) -> Option<&[String]>;
    fn locale_ctx(&self) -> Option<&LocaleContext>;
    fn registry(&self) -> Option<&Registry>;
    fn ui_locale(&self) -> Option<&str>;
    fn cache(&self) -> Option<&dyn CacheBackend>;
}

/// Post-process a single document (skip hydration -- used by find_by_id where
/// `ops::find_by_id_full` already handled hydration).
pub(crate) fn post_process_single(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    doc: &mut Document,
    opts: &impl PostProcessOpts,
    operation: &str,
) {
    let hooks = match ctx.read_hooks {
        Some(h) => h,
        None => return,
    };
    let def = ctx.collection_def();
    let slug = ctx.slug;
    let user = ctx.user;

    if opts.depth() > 0
        && let Some(registry) = opts.registry()
    {
        let mut visited = HashSet::new();
        let pop_ctx = query::PopulateContext::new(conn, registry, slug, def);
        let mut pop_opts = query::PopulateOpts::new(opts.depth());
        if let Some(s) = opts.select() {
            pop_opts = pop_opts.select(s);
        }
        if let Some(lc) = opts.locale_ctx() {
            pop_opts = pop_opts.locale_ctx(lc);
        }

        let pop_result = if let Some(cache) = opts.cache() {
            query::populate_relationships_cached(&pop_ctx, doc, &mut visited, &pop_opts, cache)
        } else {
            query::populate_relationships(&pop_ctx, doc, &mut visited, &pop_opts)
        };

        if let Err(e) = pop_result {
            warn!("populate error for {slug}/{}: {e:#}", doc.id);
        }
    }

    if let Some(ref upload_config) = def.upload
        && upload_config.enabled
    {
        upload::assemble_sizes_object(doc, upload_config);
    }

    if let Some(sel) = opts.select() {
        query::apply_select_to_document(doc, sel);
    }

    let mut denied = hooks.field_read_denied(&def.fields, user);
    denied.extend(helpers::collect_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&denied);

    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: slug,
        operation,
        user,
        ui_locale: opts.ui_locale(),
    };

    // Swap in a placeholder, run hooks, swap back
    let placeholder = Document::new("".to_string());
    let owned = mem::replace(doc, placeholder);
    *doc = hooks.after_read_one(&ar_ctx, owned);
}

/// Shared post-processing for find: hydrate, populate, upload sizes,
/// select stripping, field-level access stripping, and after_read hooks.
pub(crate) fn post_process_docs(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    docs: &mut Vec<Document>,
    opts: &impl PostProcessOpts,
) {
    let hooks = match ctx.read_hooks {
        Some(h) => h,
        None => return,
    };
    let def = ctx.collection_def();
    let slug = ctx.slug;
    let user = ctx.user;

    if opts.hydrate() {
        for doc in docs.iter_mut() {
            if let Err(e) = query::hydrate_document(
                conn,
                slug,
                &def.fields,
                doc,
                opts.select(),
                opts.locale_ctx(),
            ) {
                warn!("hydrate error for {slug}/{}: {e:#}", doc.id);
            }
        }
    }

    if opts.depth() > 0
        && let Some(registry) = opts.registry()
    {
        let pop_ctx = query::PopulateContext::new(conn, registry, slug, def);
        let mut pop_opts = query::PopulateOpts::new(opts.depth());

        if let Some(s) = opts.select() {
            pop_opts = pop_opts.select(s);
        }

        if let Some(lc) = opts.locale_ctx() {
            pop_opts = pop_opts.locale_ctx(lc);
        }

        let pop_result = if let Some(cache) = opts.cache() {
            query::populate_relationships_batch_cached(&pop_ctx, docs, &pop_opts, cache)
        } else {
            query::populate_relationships_batch(&pop_ctx, docs, &pop_opts)
        };

        if let Err(e) = pop_result {
            warn!("populate error for {slug}: {e:#}");
        }
    }

    if let Some(ref upload_config) = def.upload
        && upload_config.enabled
    {
        for doc in docs.iter_mut() {
            upload::assemble_sizes_object(doc, upload_config);
        }
    }

    if let Some(sel) = opts.select() {
        for doc in docs.iter_mut() {
            query::apply_select_to_document(doc, sel);
        }
    }

    let mut denied = hooks.field_read_denied(&def.fields, user);
    denied.extend(helpers::collect_hidden_field_names(&def.fields, ""));

    if !denied.is_empty() {
        for doc in docs.iter_mut() {
            doc.strip_fields(&denied);
        }
    }

    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: slug,
        operation: "find",
        user,
        ui_locale: opts.ui_locale(),
    };

    let processed = hooks.after_read_many(&ar_ctx, mem::take(docs));
    *docs = processed;
}
