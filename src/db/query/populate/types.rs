//! Type definitions for the populate subsystem.

use anyhow::Result;

use crate::core::cache::CacheBackend;
use crate::core::{CollectionDefinition, Document, HookRef, Registry};
use crate::db::query::AccessResult;
use crate::db::query::populate::{CachedDoc, Singleflight};
use crate::db::{DbConnection, LocaleContext, LocaleMode};

/// Minimal access-check abstraction used by join-field population.
///
/// `populate_join_docs` fetches documents from the *target* collection, which
/// has its own read-access hook. We must honor that hook so a user who is
/// denied direct reads on the target can't exfiltrate data via a virtual
/// reverse-lookup join field on another collection.
///
/// Implemented in the service layer (see `service::hooks::ReadHooks`) — kept
/// as a narrow trait here to avoid a `db -> service` dependency.
pub trait JoinAccessCheck {
    /// Check read access for the target collection.
    ///
    /// `access` is the target collection's `access.read` hook ref (carrying any
    /// per-config `options`). Implementations return `Allowed`, `Denied`, or
    /// `Constrained(filters)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the implementing access hook raises (e.g. a Lua runtime error).
    fn check(
        &self,
        access: Option<&HookRef>,
        user: Option<&Document>,
        collection: &str,
    ) -> Result<AccessResult>;
}

/// Build the shared-cache key for a **raw** (unpopulated) target document.
///
/// Format: `populate:{collection}:{id}[:{locale}]`. The cached value is the raw
/// document content, which is independent of the requesting user and of draft
/// visibility — the per-request view-access decision (`read` for published,
/// `draft` for draft rows) is applied *after* retrieval, on every read. That
/// keeps one cache entry per document shared across all users (the DB fetch is
/// deduplicated for everyone) without ever caching a user-specific view.
pub(crate) fn populate_cache_key(collection: &str, id: &str, locale: Option<&str>) -> String {
    match locale {
        Some(l) => format!("populate:{collection}:{id}:{l}"),
        None => format!("populate:{collection}:{id}"),
    }
}

/// Derive the locale portion of the cache key from an optional `LocaleContext`.
///
/// Returns:
/// - `None` when no locale context is active (unlocalized request).
/// - `Some("_default_")` for `LocaleMode::Default`.
/// - `Some("_all_")` for `LocaleMode::All`.
/// - `Some(locale_string)` for `LocaleMode::Single(locale_string)`.
pub(crate) fn locale_cache_key(locale_ctx: Option<&LocaleContext>) -> Option<String> {
    locale_ctx.map(|lc| match &lc.mode {
        LocaleMode::Single(s) => s.clone(),
        LocaleMode::Default => "_default_".to_string(),
        LocaleMode::All => "_all_".to_string(),
    })
}

/// Bundled parameters for inner population helpers, reducing argument count.
///
/// Carries the connection, registry, effective depth, locale context, cache,
/// and singleflight that every recursive population function needs. The
/// remaining per-call params (doc/docs, `field_name`, `rel_collection`, `rel_def`,
/// visited) stay as regular args.
pub(crate) struct PopulateCtx<'a> {
    pub conn: &'a dyn DbConnection,
    pub registry: &'a Registry,
    pub effective_depth: i32,
    /// The id of the document whose (possibly nested) fields are being
    /// populated. A `Join` field reverse-looks-up the target collection on this
    /// id regardless of how deeply it is nested, so the nested-container walker
    /// needs it. Empty for the batch flat-relationship context, which populates
    /// a field across many docs and never resolves a join.
    pub root_id: &'a str,
    pub locale_ctx: Option<&'a LocaleContext>,
    /// When true, drafts were *not* requested (the reader did not opt into
    /// drafts), so draft target rows are hidden from population regardless of
    /// access. Threaded from the parent read's `include_drafts`. The "drafts
    /// requested" axis of the requested×allowed rule in `target_row_visible`.
    pub published_only: bool,
    pub cache: &'a dyn CacheBackend,
    /// Deduplicates concurrent cache-miss fetches for the same target. The
    /// top-level entry point constructs this fresh per populate call; service
    /// layers may in future share a process-wide singleflight here to dedupe
    /// across concurrent requests.
    pub singleflight: &'a Singleflight<CachedDoc>,
    /// Target-collection `read` access check. When `Some`, every relationship
    /// target is gated by the target collection's `access.read` (honoring any
    /// row-level `Constrained` filter); when `None` (legacy/internal callers)
    /// population proceeds without a target access check.
    pub join_access: Option<&'a dyn JoinAccessCheck>,
    /// Current user for the access check. Only consulted alongside `join_access`.
    pub user: Option<&'a Document>,
}

/// Collection and registry context for population.
pub struct PopulateContext<'a> {
    pub(crate) conn: &'a dyn DbConnection,
    pub(crate) registry: &'a Registry,
    pub(crate) collection_slug: &'a str,
    pub(crate) def: &'a CollectionDefinition,
}

impl<'a> PopulateContext<'a> {
    pub fn new(
        conn: &'a dyn DbConnection,
        registry: &'a Registry,
        collection_slug: &'a str,
        def: &'a CollectionDefinition,
    ) -> Self {
        Self {
            conn,
            registry,
            collection_slug,
            def,
        }
    }
}

/// Options controlling population behavior.
#[derive(Default)]
pub struct PopulateOpts<'a> {
    pub(crate) depth: i32,
    pub(crate) select: Option<&'a [String]>,
    pub(crate) locale_ctx: Option<&'a LocaleContext>,
    /// When true, draft target documents are excluded from population.
    /// Set by the service read layer from `!include_drafts`. Defaults to
    /// false (drafts visible) for internal/legacy callers.
    pub(crate) published_only: bool,
    /// Optional access-check for join-field target collections. When `None`,
    /// join population proceeds without a target-collection access check
    /// (legacy / internal callers). When `Some`, the check is invoked for
    /// each join field and may deny or constrain the underlying find.
    pub(crate) join_access: Option<&'a dyn JoinAccessCheck>,
    /// Current user for the access check. Only consulted when
    /// `join_access` is also set.
    pub(crate) user: Option<&'a Document>,
}

impl<'a> PopulateOpts<'a> {
    #[must_use]
    pub fn new(depth: i32) -> Self {
        Self {
            depth,
            select: None,
            locale_ctx: None,
            published_only: false,
            join_access: None,
            user: None,
        }
    }

    /// Hide draft target documents from population (reader is not allowed
    /// to see drafts). Threads the parent read's `!include_drafts`.
    #[must_use]
    pub fn published_only(mut self, published_only: bool) -> Self {
        self.published_only = published_only;
        self
    }

    #[must_use]
    pub fn select(mut self, select: &'a [String]) -> Self {
        self.select = Some(select);
        self
    }

    #[must_use]
    pub fn locale_ctx(mut self, ctx: &'a LocaleContext) -> Self {
        self.locale_ctx = Some(ctx);
        self
    }

    /// Attach an access-check for join-field target collections plus the
    /// current user. Both must be set together to enable the check.
    #[must_use]
    pub fn join_access(
        mut self,
        check: &'a dyn JoinAccessCheck,
        user: Option<&'a Document>,
    ) -> Self {
        self.join_access = Some(check);
        self.user = user;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn populate_cache_key_no_locale() {
        assert_eq!(populate_cache_key("posts", "p1", None), "populate:posts:p1");
    }

    #[test]
    fn populate_cache_key_with_locale() {
        assert_eq!(
            populate_cache_key("posts", "p1", Some("de")),
            "populate:posts:p1:de"
        );
    }

    #[test]
    fn locale_cache_key_none_without_context() {
        assert_eq!(locale_cache_key(None), None);
    }

    #[test]
    fn locale_cache_key_single_locale() {
        let config = crate::config::LocaleConfig {
            locales: vec!["en".to_string(), "de".to_string()],
            default_locale: "en".to_string(),
            fallback: true,
        };
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config,
        };
        assert_eq!(locale_cache_key(Some(&ctx)), Some("de".to_string()));
    }

    #[test]
    fn locale_cache_key_default_mode() {
        let config = crate::config::LocaleConfig {
            locales: vec!["en".to_string(), "de".to_string()],
            default_locale: "en".to_string(),
            fallback: true,
        };
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config,
        };
        assert_eq!(locale_cache_key(Some(&ctx)), Some("_default_".to_string()));
    }

    #[test]
    fn locale_cache_key_all_mode() {
        let config = crate::config::LocaleConfig {
            locales: vec!["en".to_string(), "de".to_string()],
            default_locale: "en".to_string(),
            fallback: true,
        };
        let ctx = LocaleContext {
            mode: LocaleMode::All,
            config,
        };
        assert_eq!(locale_cache_key(Some(&ctx)), Some("_all_".to_string()));
    }
}
