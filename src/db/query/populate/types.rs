//! Type definitions for the populate subsystem.

use std::collections::HashSet;

use anyhow::Result;

use crate::core::cache::CacheBackend;
use crate::core::{
    Builder, CollectionDefinition, Document, FieldDefinition, HookRef, JoinConfig, Registry,
};
use crate::db::query::populate::{CachedDoc, Singleflight};
use crate::db::query::{AccessResult, ReadLocale};
use crate::db::{DbConnection, LocaleContext};

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

    /// For each of `children` — documents of `target` a join lists, each as
    /// the reader will see it — whether its join `on` value survives the
    /// reader's field read strip (`hidden`, and `access.read` judged with that
    /// child as `ctx.document`). A child's presence in a join IS that value, so
    /// a join lists only children for which this holds; deciding it before the
    /// join's `limit` is applied keeps the limit counting listed children.
    ///
    /// The default keeps every child — for an implementation without a field
    /// read strip. The strip applied to the populated result stays the
    /// backstop either way.
    fn on_readable(&self, _join: &JoinReaders<'_>, children: &[Document]) -> Vec<bool> {
        vec![true; children.len()]
    }
}

/// Who reads a join's children, and through which join: the input to
/// [`JoinAccessCheck::on_readable`].
#[derive(Builder)]
pub struct JoinReaders<'a> {
    #[builder(required)]
    pub join: &'a JoinConfig,
    #[builder(required)]
    pub target: &'a CollectionDefinition,
    pub user: Option<&'a Document>,
    /// The locale field access is judged at.
    pub locale: Option<&'a str>,
}

/// A populate cycle guard: the `(collection, id)` pairs on the current path —
/// the documents between the one being populated and the read's top level
/// (itself included). A reference to a document on the path stays an id; one
/// merely populated elsewhere in the tree does not.
pub(crate) type Visited = HashSet<(String, String)>;

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

/// Marks an all-locales read. `*` is not a legal locale code (the locale config
/// allows only ASCII alphanumerics, `-` and `_`), so no locale's key can spell
/// this one — a marker built from legal characters would collide with a locale
/// named after it.
const ALL_LOCALES_KEY: &str = "*";

/// Separates the read locale from the locale it falls back to. Rejected in a
/// locale code for the same reason as [`ALL_LOCALES_KEY`], so `en>de` can only
/// ever mean "read `en`, fall back to `de`".
const FALLBACK_SEPARATOR: char = '>';

/// Derive the locale portion of the cache key from an optional `LocaleContext`,
/// so reads that select the same data share one key — and only those.
///
/// Keyed off the read locale ([`LocaleContext::read_locale`]), which is the
/// whole of what a cached document's content depends on: the locale its values
/// come from and the locale standing in while that one holds nothing. A default
/// read, a read of the default locale and a read of a locale that isn't
/// configured all read the default locale, and share its key; the same locale
/// read with and without fallback do not.
///
/// Returns `None` when no locale context is active (unlocalized request).
pub(crate) fn locale_cache_key(locale_ctx: Option<&LocaleContext>) -> Option<String> {
    locale_ctx.map(|lc| match lc.read_locale() {
        None => ALL_LOCALES_KEY.to_string(),
        Some(ReadLocale {
            locale,
            fallback: None,
        }) => locale.to_string(),
        Some(ReadLocale {
            locale,
            fallback: Some(fallback),
        }) => format!("{locale}{FALLBACK_SEPARATOR}{fallback}"),
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

/// The document being populated: its connection and registry, the slug its
/// documents are told apart by on a populate path (a collection's slug, or a
/// global's table), and its field definitions.
pub struct PopulateContext<'a> {
    pub(crate) conn: &'a dyn DbConnection,
    pub(crate) registry: &'a Registry,
    pub(crate) collection_slug: &'a str,
    pub(crate) fields: &'a [FieldDefinition],
}

impl<'a> PopulateContext<'a> {
    pub fn new(
        conn: &'a dyn DbConnection,
        registry: &'a Registry,
        collection_slug: &'a str,
        fields: &'a [FieldDefinition],
    ) -> Self {
        Self {
            conn,
            registry,
            collection_slug,
            fields,
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
    use crate::{config::LocaleConfig, db::LocaleMode};

    fn en_de() -> LocaleConfig {
        LocaleConfig {
            locales: vec!["en".to_string(), "de".to_string()],
            default_locale: "en".to_string(),
            fallback: true,
        }
    }

    fn single(locale: &str, config: LocaleConfig) -> LocaleContext {
        LocaleContext {
            mode: LocaleMode::Single(locale.to_string()),
            config,
        }
    }

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
        let no_fallback = LocaleConfig {
            fallback: false,
            ..en_de()
        };
        assert_eq!(
            locale_cache_key(Some(&single("de", no_fallback))),
            Some("de".to_string())
        );
    }

    /// Regression: a locale that isn't configured reads as the default locale,
    /// yet keyed its own cache entry — the same read cached twice.
    #[test]
    fn locale_cache_key_an_unconfigured_locale_shares_the_default_key() {
        assert_eq!(
            locale_cache_key(Some(&single("fr", en_de()))),
            locale_cache_key(Some(&single("en", en_de())))
        );
    }

    /// Regression: the key ignored `fallback`, so the same locale read with the
    /// default locale standing in for its empty values and without it shared one
    /// entry — whichever read filled the cache decided what the other saw.
    #[test]
    fn locale_cache_key_separates_a_read_with_fallback_from_one_without() {
        let with_fallback = locale_cache_key(Some(&single("de", en_de())));
        let without = locale_cache_key(Some(&single(
            "de",
            LocaleConfig {
                fallback: false,
                ..en_de()
            },
        )));

        assert_eq!(with_fallback, Some("de>en".to_string()));
        assert_eq!(without, Some("de".to_string()));
    }

    /// A read of the default locale takes no fallback (it *is* the fallback),
    /// so it keys exactly like the default read.
    #[test]
    fn locale_cache_key_default_locale_shares_the_default_read_key() {
        let default_read = LocaleContext {
            mode: LocaleMode::Default,
            config: en_de(),
        };

        assert_eq!(
            locale_cache_key(Some(&single("en", en_de()))),
            locale_cache_key(Some(&default_read))
        );
    }

    /// Regression: the all-locales marker was `_all_`, a legal locale code — a
    /// locale named `_all_` shared the all-locales entry and read the nested
    /// per-locale shape as its own scalar value.
    #[test]
    fn locale_cache_key_all_mode_cannot_collide_with_a_locale() {
        let config = LocaleConfig {
            locales: vec!["en".to_string(), "_all_".to_string()],
            default_locale: "en".to_string(),
            fallback: false,
        };
        let all = LocaleContext {
            mode: LocaleMode::All,
            config: config.clone(),
        };

        assert_ne!(
            locale_cache_key(Some(&all)),
            locale_cache_key(Some(&single("_all_", config)))
        );
    }

    /// A default read selects the default locale's data, so it shares that
    /// locale's key.
    #[test]
    fn locale_cache_key_default_mode_shares_the_default_locale_key() {
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: en_de(),
        };
        assert_eq!(locale_cache_key(Some(&ctx)), Some("en".to_string()));
    }

    #[test]
    fn locale_cache_key_all_mode() {
        let ctx = LocaleContext {
            mode: LocaleMode::All,
            config: en_de(),
        };
        assert_eq!(locale_cache_key(Some(&ctx)), Some("*".to_string()));
    }
}
