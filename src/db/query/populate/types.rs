//! Type definitions for the populate subsystem.

use crate::core::cache::CacheBackend;
use crate::core::{CollectionDefinition, Registry};
use crate::db::{DbConnection, LocaleContext, LocaleMode};

/// Build a cache key for a populated document.
///
/// Format: `populate:{collection}:{id}` or `populate:{collection}:{id}:{locale}`
pub fn populate_cache_key(collection: &str, id: &str, locale: Option<&str>) -> String {
    match locale {
        Some(l) => format!("populate:{}:{}:{}", collection, id, l),
        None => format!("populate:{}:{}", collection, id),
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
/// Carries the connection, registry, effective depth, locale context, and cache
/// that every recursive population function needs. The remaining per-call params
/// (doc/docs, field_name, rel_collection, rel_def, visited) stay as regular args.
pub(crate) struct PopulateCtx<'a> {
    pub conn: &'a dyn DbConnection,
    pub registry: &'a Registry,
    pub effective_depth: i32,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub cache: &'a dyn CacheBackend,
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
pub struct PopulateOpts<'a> {
    pub(crate) depth: i32,
    pub(crate) select: Option<&'a [String]>,
    pub(crate) locale_ctx: Option<&'a LocaleContext>,
}

impl<'a> PopulateOpts<'a> {
    pub fn new(depth: i32) -> Self {
        Self {
            depth,
            select: None,
            locale_ctx: None,
        }
    }

    pub fn select(mut self, select: &'a [String]) -> Self {
        self.select = Some(select);
        self
    }

    pub fn locale_ctx(mut self, ctx: &'a LocaleContext) -> Self {
        self.locale_ctx = Some(ctx);
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
