//! Shared post-processing for read operations (populate, upload sizes,
//! select stripping, field-level access, `after_read` hooks).

use std::{collections::HashSet, mem, slice};

use tracing::warn;

use crate::{
    core::{
        Builder, CollectionDefinition, Document, FieldDefinition, Registry, ReqContext,
        cache::{CacheBackend, NoneCache},
        upload,
    },
    db::{CachedDoc, DbConnection, LocaleContext, SharedPopulateSingleflight, Singleflight, query},
    hooks::lifecycle::AfterReadCtx,
    service::{
        ReadHooks, ReadStripArgs, ServiceContext, helpers, hooks::ReadHooksJoinGuard,
        read::populated_strip::EmbeddedDocStripper,
    },
};

/// Per-call fields needed by post-processing. Implemented by all read input
/// structs. Infrastructure (registry, populate cache, singleflight) is read
/// from the `ServiceContext` instead — inputs carry only per-call data.
pub(crate) trait PostProcessOpts {
    fn depth(&self) -> i32;
    /// Whether this read is allowed to see draft documents. When false,
    /// relationship population hides draft targets (parity with the
    /// service-layer `_status = 'published'` filter on the top-level read).
    fn include_drafts(&self) -> bool;
    fn hydrate(&self) -> bool;
    fn select(&self) -> Option<&[String]>;
    fn locale_ctx(&self) -> Option<&LocaleContext>;
}

/// The per-call inputs of one post-processing pass: the read's options, the
/// operation name `after_read` hooks see, and the request context
/// `before_read` produced.
#[derive(Builder)]
pub(crate) struct PostProcessCall<'a, O> {
    #[builder(required)]
    opts: &'a O,
    #[builder(required)]
    operation: &'a str,
    #[builder(required)]
    req_context: ReqContext,
}

/// Post-process a single document (skip hydration -- used by `find_by_id` where
/// `ops::find_by_id_full` already handled hydration).
pub(crate) fn post_process_single<O: PostProcessOpts>(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    doc: &mut Document,
    call: PostProcessCall<'_, O>,
) {
    let (Some(hooks), Ok(def)) = (ctx.read_hooks, ctx.collection_def()) else {
        return;
    };
    let opts = call.opts;
    let access_locale = opts.locale_ctx().map(LocaleContext::access_locale);

    populate_one(ctx, conn, doc, opts);
    shape_for_read(def, opts, slice::from_mut(doc));

    // Data-aware field-read strip (per-row `ctx.data`, full-doc `ctx.document`),
    // then the document-independent API-hidden strip.
    helpers::strip_unreadable(
        hooks,
        &ReadStripArgs::builder(&def.fields, ctx.slug)
            .user(ctx.user)
            .locale(access_locale)
            .build(),
        doc,
    );

    // Strip field-read-denied fields from populated relationship targets — each
    // embedded doc belongs to another collection with its own field access.
    if let Some(registry) = ctx.registry {
        EmbeddedDocStripper::new(registry, hooks, ctx.user, access_locale).strip(doc, &def.fields);
    }

    let ar_ctx = after_read_ctx(ctx, def, call);
    let owned = mem::replace(doc, Document::new(String::new()));
    *doc = hooks.after_read_one(&ar_ctx, owned);
}

/// Who and what a batch read strip evaluates field-read access for.
/// `registry` is `None` when populated relationship targets need no strip.
#[derive(Builder)]
struct ReadStrip<'a> {
    #[builder(required)]
    fields: &'a [FieldDefinition],
    #[builder(required)]
    hooks: &'a dyn ReadHooks,
    #[builder(required)]
    collection: &'a str,
    user: Option<&'a Document>,
    locale: Option<&'a str>,
    registry: Option<&'a Registry>,
}

impl ReadStrip<'_> {
    /// Strip read-denied + API-hidden fields from a batch of documents: first
    /// the documents' own fields, then the field-access-denied fields of any
    /// populated relationship targets (via [`EmbeddedDocStripper`], which
    /// memoizes per-target denials across the batch).
    fn strip_docs(&self, docs: &mut [Document]) {
        // Field-read access is data-aware (per-doc, per-row), so evaluate it on
        // each document — but in ONE batch so the Lua VM is acquired once for
        // the whole list, not per doc.
        helpers::strip_unreadable_docs(
            self.hooks,
            &ReadStripArgs::builder(self.fields, self.collection)
                .user(self.user)
                .locale(self.locale)
                .build(),
            docs,
        );

        if let Some(registry) = self.registry {
            let stripper = EmbeddedDocStripper::new(registry, self.hooks, self.user, self.locale);
            for doc in docs.iter_mut() {
                stripper.strip(doc, self.fields);
            }
        }
    }
}

/// Shared post-processing for find: hydrate, populate, upload sizes,
/// select stripping, field-level access stripping, and `after_read` hooks.
pub(crate) fn post_process_docs<O: PostProcessOpts>(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    docs: &mut Vec<Document>,
    call: PostProcessCall<'_, O>,
) {
    let (Some(hooks), Ok(def)) = (ctx.read_hooks, ctx.collection_def()) else {
        return;
    };
    let opts = call.opts;

    hydrate_many(ctx, conn, docs, opts);
    populate_many(ctx, conn, docs, opts);
    shape_for_read(def, opts, docs);

    ReadStrip::builder(&def.fields, hooks, ctx.slug)
        .user(ctx.user)
        .locale(opts.locale_ctx().map(LocaleContext::access_locale))
        .registry(ctx.registry)
        .build()
        .strip_docs(docs);

    let ar_ctx = after_read_ctx(ctx, def, call);
    *docs = hooks.after_read_many(&ar_ctx, mem::take(docs));
}

/// Batched plural-doc hydrate: one `WHERE parent_id IN (…)` SELECT per
/// top-level has-many relationship field instead of one per (doc, field).
/// Non-batched field shapes (Array, Blocks, fields nested in Group/Tabs) still
/// go per-doc inside `hydrate_documents`.
fn hydrate_many<O: PostProcessOpts>(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    docs: &mut [Document],
    opts: &O,
) {
    let (true, Ok(def)) = (opts.hydrate(), ctx.collection_def()) else {
        return;
    };

    let hydrated = query::hydrate_documents(
        conn,
        ctx.slug,
        &def.fields,
        docs,
        opts.select(),
        opts.locale_ctx(),
    );

    if let Err(e) = hydrated {
        warn!("hydrate error for {}: {e:#}", ctx.slug);
    }
}

/// Populate one document's relationships to the read's depth.
fn populate_one<O: PostProcessOpts>(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    doc: &mut Document,
    opts: &O,
) {
    let (Some(hooks), Ok(def), Some(registry)) =
        (ctx.read_hooks, ctx.collection_def(), ctx.registry)
    else {
        return;
    };

    if opts.depth() <= 0 {
        return;
    }

    let pop_ctx = query::PopulateContext::new(conn, registry, ctx.slug, def);
    let guard = ReadHooksJoinGuard::new(hooks);
    let pop_opts = populate_opts(opts, &guard, ctx.user);
    let mut visited = HashSet::new();

    let populated = with_populate_state(ctx, |cache, singleflight| {
        query::populate_relationships_cached_with_singleflight(
            &pop_ctx,
            doc,
            &mut visited,
            &pop_opts,
            cache,
            singleflight,
        )
    });

    if let Err(e) = populated {
        warn!("populate error for {}/{}: {e:#}", ctx.slug, doc.id);
    }
}

/// Populate a batch of documents' relationships to the read's depth.
fn populate_many<O: PostProcessOpts>(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    docs: &mut [Document],
    opts: &O,
) {
    let (Some(hooks), Ok(def), Some(registry)) =
        (ctx.read_hooks, ctx.collection_def(), ctx.registry)
    else {
        return;
    };

    if opts.depth() <= 0 {
        return;
    }

    let pop_ctx = query::PopulateContext::new(conn, registry, ctx.slug, def);
    let guard = ReadHooksJoinGuard::new(hooks);
    let pop_opts = populate_opts(opts, &guard, ctx.user);

    let populated = with_populate_state(ctx, |cache, singleflight| {
        query::populate_relationships_batch_cached_with_singleflight(
            &pop_ctx,
            docs,
            &pop_opts,
            cache,
            singleflight,
        )
    });

    if let Err(e) = populated {
        warn!("populate error for {}: {e:#}", ctx.slug);
    }
}

/// The populate options of a read: its depth, draft visibility, select and
/// locale, with join targets checked against the reader's access.
fn populate_opts<'a, O: PostProcessOpts>(
    opts: &'a O,
    guard: &'a ReadHooksJoinGuard<'a>,
    user: Option<&'a Document>,
) -> query::PopulateOpts<'a> {
    let mut pop_opts =
        query::PopulateOpts::new(opts.depth()).published_only(!opts.include_drafts());

    if let Some(select) = opts.select() {
        pop_opts = pop_opts.select(select);
    }

    if let Some(locale_ctx) = opts.locale_ctx() {
        pop_opts = pop_opts.locale_ctx(locale_ctx);
    }

    pop_opts.join_access(guard, user)
}

/// Run a populate with the effective cache and singleflight. Access-leak
/// guardrail: an override-access context gets neither the shared cache nor the
/// process-wide singleflight (see [`effective_populate_state`]). The shared
/// singleflight dedups cache misses across concurrent requests; without one a
/// fresh per-call singleflight is used, and without a cache, none.
fn with_populate_state<R>(
    ctx: &ServiceContext,
    run: impl FnOnce(&dyn CacheBackend, &Singleflight<CachedDoc>) -> R,
) -> R {
    let (cache, shared_singleflight) = effective_populate_state(ctx);

    let fallback;
    let singleflight: &Singleflight<CachedDoc> = if let Some(shared) = shared_singleflight {
        shared.as_ref()
    } else {
        fallback = Singleflight::new();
        &fallback
    };

    run(cache.unwrap_or(&NoneCache), singleflight)
}

/// Shape read documents for the caller: an upload's per-size values folded
/// into `sizes`, then the read's `select` applied.
fn shape_for_read<O: PostProcessOpts>(def: &CollectionDefinition, opts: &O, docs: &mut [Document]) {
    for doc in docs.iter_mut() {
        upload::shape_read_document(def, doc);

        if let Some(select) = opts.select() {
            query::apply_select_to_document(doc, select);
        }
    }
}

/// The `after_read` context of a post-processing pass.
fn after_read_ctx<'a, O: PostProcessOpts>(
    ctx: &'a ServiceContext,
    def: &'a CollectionDefinition,
    call: PostProcessCall<'a, O>,
) -> AfterReadCtx<'a> {
    AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: ctx.slug,
        operation: call.operation,
        // `hook_locale` (not `access_locale`): in All-locale mode the value is
        // a per-locale map, so `ctx.locale` is None rather than a misleading
        // single locale — see the doc on `LocaleContext::hook_locale`.
        locale: call.opts.locale_ctx().and_then(LocaleContext::hook_locale),
        user: ctx.user,
        ui_locale: ctx.ui_locale.as_deref(),
        context: call.req_context,
    }
}

/// Resolve the effective populate cache + singleflight from the context,
/// applying the access-leak guardrail: when the calling `ServiceContext` is in
/// `override_access` mode (MCP, Lua `opts.overrideAccess = true`), neither the
/// shared populate cache nor the process-wide singleflight may be used —
/// regardless of what the context carries.
///
/// Rationale (see `validate_filters.rs` header for the full note): override
/// callers bypass collection-level access hooks. Sharing the populate cache
/// with regular requests would let a doc fetched under override-access leak
/// into another user's populate lookup, because a subsequent cache hit does
/// not re-run the access check that decided the original fetch was allowed.
/// Zeroing both is a single-chokepoint rule that covers every populate entry
/// point (find, `find_by_id`).
///
/// Override-access fetches are still deduplicated *within* their own call via
/// the fresh per-call singleflight created by `populate_relationships_*`.
fn effective_populate_state<'c>(
    ctx: &'c ServiceContext,
) -> (
    Option<&'c dyn CacheBackend>,
    Option<&'c SharedPopulateSingleflight>,
) {
    if ctx.override_access {
        return (None, None);
    }
    (ctx.cache.as_deref(), ctx.populate_singleflight.as_ref())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    use crate::{
        core::{CollectionDefinition, SharedCache, cache::MemoryCache},
        db::query::Singleflight,
        service::ServiceContext,
    };

    /// Guardrail: when `override_access = true`, the effective cache and
    /// singleflight are forced to `None`, even if the context carries a
    /// shared cache + singleflight. This prevents cache-leak across users.
    #[test]
    fn override_access_forces_no_shared_cache_or_singleflight() {
        let def = CollectionDefinition::new("posts");
        let cache: SharedCache = Arc::new(MemoryCache::new(0));
        let sf: SharedPopulateSingleflight = Arc::new(Singleflight::new());
        let ctx = ServiceContext::collection("posts", &def)
            .override_access(true)
            .cache(Some(cache))
            .populate_singleflight(Some(sf))
            .build();

        let (effective_cache, effective_sf) = effective_populate_state(&ctx);
        assert!(
            effective_cache.is_none(),
            "cache must be zeroed under override_access"
        );
        assert!(
            effective_sf.is_none(),
            "singleflight must be zeroed under override_access"
        );
    }

    /// Without `override_access`, the context's cache + singleflight are passed
    /// through unchanged so normal requests still benefit from cross-request
    /// dedup and the shared populate cache.
    #[test]
    fn no_override_access_passes_through_cache_and_singleflight() {
        let def = CollectionDefinition::new("posts");
        let cache: SharedCache = Arc::new(MemoryCache::new(0));
        let sf: SharedPopulateSingleflight = Arc::new(Singleflight::new());
        let ctx = ServiceContext::collection("posts", &def)
            .cache(Some(cache))
            .populate_singleflight(Some(sf.clone()))
            .build();
        assert!(!ctx.override_access);

        let (effective_cache, effective_sf) = effective_populate_state(&ctx);
        assert!(
            effective_cache.is_some(),
            "cache should be threaded through"
        );
        assert!(
            effective_sf.is_some_and(|s| Arc::ptr_eq(s, &sf)),
            "singleflight should be the context's Arc"
        );
    }

    /// When the context carries no cache/singleflight at all, the effective
    /// state is `None` regardless of `override_access`.
    #[test]
    fn no_cache_and_no_singleflight_stays_none() {
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def).build();

        let (effective_cache, effective_sf) = effective_populate_state(&ctx);
        assert!(effective_cache.is_none());
        assert!(effective_sf.is_none());
    }
}
