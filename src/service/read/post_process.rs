//! Shared post-processing for read operations (populate, upload sizes,
//! select stripping, field-level access, after_read hooks).

use std::{collections::HashSet, mem};

use tracing::warn;

use crate::{
    core::{
        Document, Registry,
        cache::{CacheBackend, NoneCache},
        upload,
    },
    db::{
        DbConnection, LocaleContext,
        query::{self, SharedPopulateSingleflight, Singleflight},
    },
    hooks::lifecycle::AfterReadCtx,
    service::{ServiceContext, helpers, hooks::ReadHooksJoinGuard},
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
    /// Process-wide singleflight for deduplicating concurrent populate
    /// cache-miss DB fetches across requests. `None` falls back to a
    /// fresh per-call singleflight.
    fn singleflight(&self) -> Option<&SharedPopulateSingleflight>;
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
        let guard = ReadHooksJoinGuard::new(hooks);
        pop_opts = pop_opts.join_access(&guard, user);

        // Access-leak guardrail: override-access callers (Lua opts.overrideAccess,
        // MCP) bypass collection access hooks. They must not share the populate
        // cache or singleflight with regular requests, otherwise a doc fetched
        // under override-access could leak into another user's populate cache
        // lookup even when their access evaluation differs. See
        // `validate_filters.rs` for the full rationale.
        let (effective_cache, effective_singleflight) = effective_populate_state(ctx, opts);

        // Thread through the shared process-wide singleflight when the caller
        // provided one so concurrent populates across requests dedup cache
        // misses. Otherwise fall back to a fresh per-call singleflight.
        let fallback_sf: Singleflight<Option<Document>>;
        let singleflight: &Singleflight<Option<Document>> = match effective_singleflight {
            Some(arc) => arc.as_ref(),
            None => {
                fallback_sf = Singleflight::new();
                &fallback_sf
            }
        };

        let pop_result = if let Some(cache) = effective_cache {
            query::populate_relationships_cached_with_singleflight(
                &pop_ctx,
                doc,
                &mut visited,
                &pop_opts,
                cache,
                singleflight,
            )
        } else {
            query::populate_relationships_cached_with_singleflight(
                &pop_ctx,
                doc,
                &mut visited,
                &pop_opts,
                &NoneCache,
                singleflight,
            )
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

        let guard = ReadHooksJoinGuard::new(hooks);
        pop_opts = pop_opts.join_access(&guard, user);

        // Access-leak guardrail — see the matching comment in
        // `post_process_single` and `validate_filters.rs` for the rationale.
        let (effective_cache, effective_singleflight) = effective_populate_state(ctx, opts);

        let fallback_sf;
        let singleflight: &Singleflight<Option<Document>> = match effective_singleflight {
            Some(arc) => arc.as_ref(),
            None => {
                fallback_sf = Singleflight::new();
                &fallback_sf
            }
        };

        let pop_result = if let Some(cache) = effective_cache {
            query::populate_relationships_batch_cached_with_singleflight(
                &pop_ctx,
                docs,
                &pop_opts,
                cache,
                singleflight,
            )
        } else {
            query::populate_relationships_batch_cached_with_singleflight(
                &pop_ctx,
                docs,
                &pop_opts,
                &NoneCache,
                singleflight,
            )
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

/// Resolve the effective populate cache + singleflight, applying the
/// access-leak guardrail: when the calling `ServiceContext` is in
/// `override_access` mode (MCP, Lua `opts.overrideAccess = true`), neither the
/// shared populate cache nor the process-wide singleflight may be used —
/// regardless of what the caller's input struct carries.
///
/// Rationale (see `validate_filters.rs` header for the full note): override
/// callers bypass collection-level access hooks. Sharing the populate cache
/// with regular requests would let a doc fetched under override-access leak
/// into another user's populate lookup, because a subsequent cache hit does
/// not re-run the access check that decided the original fetch was allowed.
/// Zeroing both is a single-chokepoint rule that covers every populate entry
/// point (find, find_by_id).
///
/// Override-access fetches are still deduplicated *within* their own call via
/// the fresh per-call singleflight created by `populate_relationships_*`.
fn effective_populate_state<'o, O: PostProcessOpts>(
    ctx: &ServiceContext,
    opts: &'o O,
) -> (
    Option<&'o dyn CacheBackend>,
    Option<&'o SharedPopulateSingleflight>,
) {
    if ctx.override_access {
        return (None, None);
    }
    (opts.cache(), opts.singleflight())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    use crate::{
        core::{
            CollectionDefinition, Registry,
            cache::{CacheBackend, MemoryCache},
        },
        db::{LocaleContext, query::Singleflight},
        service::ServiceContext,
    };

    /// Test harness implementing `PostProcessOpts` with a real cache and
    /// singleflight we can inspect.
    struct FakeOpts<'a> {
        cache: Option<&'a dyn CacheBackend>,
        singleflight: Option<SharedPopulateSingleflight>,
    }

    impl PostProcessOpts for FakeOpts<'_> {
        fn depth(&self) -> i32 {
            1
        }
        fn hydrate(&self) -> bool {
            false
        }
        fn select(&self) -> Option<&[String]> {
            None
        }
        fn locale_ctx(&self) -> Option<&LocaleContext> {
            None
        }
        fn registry(&self) -> Option<&Registry> {
            None
        }
        fn ui_locale(&self) -> Option<&str> {
            None
        }
        fn cache(&self) -> Option<&dyn CacheBackend> {
            self.cache
        }
        fn singleflight(&self) -> Option<&SharedPopulateSingleflight> {
            self.singleflight.as_ref()
        }
    }

    /// Guardrail: when `override_access = true`, the effective cache and
    /// singleflight are forced to `None`, even if the opts carry a
    /// shared cache + singleflight. This prevents cache-leak across users.
    #[test]
    fn override_access_forces_no_shared_cache_or_singleflight() {
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def)
            .override_access(true)
            .build();

        let cache = MemoryCache::new(0);
        let sf: SharedPopulateSingleflight = Arc::new(Singleflight::new());
        let opts = FakeOpts {
            cache: Some(&cache),
            singleflight: Some(sf.clone()),
        };

        let (effective_cache, effective_sf) = effective_populate_state(&ctx, &opts);
        assert!(
            effective_cache.is_none(),
            "cache must be zeroed under override_access"
        );
        assert!(
            effective_sf.is_none(),
            "singleflight must be zeroed under override_access"
        );
    }

    /// Without override_access, the caller's cache + singleflight are passed
    /// through unchanged so normal requests still benefit from cross-request
    /// dedup and the shared populate cache.
    #[test]
    fn no_override_access_passes_through_cache_and_singleflight() {
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def).build();
        assert!(!ctx.override_access);

        let cache = MemoryCache::new(0);
        let sf: SharedPopulateSingleflight = Arc::new(Singleflight::new());
        let opts = FakeOpts {
            cache: Some(&cache),
            singleflight: Some(sf.clone()),
        };

        let (effective_cache, effective_sf) = effective_populate_state(&ctx, &opts);
        assert!(
            effective_cache.is_some(),
            "cache should be threaded through"
        );
        assert!(
            effective_sf.map(|s| Arc::ptr_eq(s, &sf)).unwrap_or(false),
            "singleflight should be the caller's Arc"
        );
    }

    /// When the caller doesn't provide a cache/singleflight at all, the
    /// effective state is `None` regardless of override_access.
    #[test]
    fn no_cache_and_no_singleflight_stays_none() {
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def).build();

        let opts = FakeOpts {
            cache: None,
            singleflight: None,
        };

        let (effective_cache, effective_sf) = effective_populate_state(&ctx, &opts);
        assert!(effective_cache.is_none());
        assert!(effective_sf.is_none());
    }
}
