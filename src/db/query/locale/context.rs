//! The locale context a query runs under and the locale decisions derived
//! from it.

use anyhow::{Result, bail};

use crate::{
    config::LocaleConfig,
    db::query::helpers::{locale_column, quote_ident},
};

/// How to handle localized fields in a query.
#[derive(Debug, Clone)]
pub enum LocaleMode {
    /// Return only the default locale (or no locales if disabled). Flat field names.
    Default,
    /// Return a specific locale. Flat field names.
    Single(String),
    /// Return all locales. Nested objects: { en: "val", de: "val" }.
    All,
}

/// The stored locale a read takes a localized value from, and the locale it
/// falls back to while that one holds nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadLocale<'a> {
    pub locale: &'a str,
    pub fallback: Option<&'a str>,
}

impl<'a> ReadLocale<'a> {
    pub(crate) fn new(locale: &'a str, fallback: Option<&'a str>) -> Self {
        Self { locale, fallback }
    }

    /// The SQL expression a read takes `column`'s value from: the reading
    /// locale's column, wrapped in `COALESCE` with the fallback locale's while
    /// the reading one holds nothing.
    ///
    /// The SELECT's value, a filter's comparand, the ORDER BY key and the
    /// keyset cursor's comparand are all this one expression. A surface that
    /// compared the bare `title__de` while the SELECT returned
    /// `COALESCE(title__de, title__en)` disagreed with the values it listed: a
    /// document shown with its fallback title was missed by a filter on that
    /// title, sorted as NULL, and skipped or repeated across pages.
    ///
    /// # Errors
    ///
    /// Returns an error if a locale code has no column form.
    pub(crate) fn column_expr(&self, column: &str) -> Result<String> {
        let read = quote_ident(&locale_column(column, self.locale)?);

        let Some(fallback) = self.fallback else {
            return Ok(read);
        };

        let fallback = quote_ident(&locale_column(column, fallback)?);

        Ok(format!("COALESCE({read}, {fallback})"))
    }
}

/// Locale context for query functions: combines config + mode.
#[derive(Debug, Clone)]
pub struct LocaleContext {
    pub mode: LocaleMode,
    pub config: LocaleConfig,
}

impl LocaleContext {
    /// The default-locale context for `config`, or `None` when localization
    /// is disabled. Every internal read of a possibly-localized row goes
    /// through this: a `None` context on a localized collection selects bare
    /// column names that do not exist (`title` vs `title__en`) and errors.
    #[must_use]
    pub fn default_for(config: &LocaleConfig) -> Option<Self> {
        if !config.is_enabled() {
            return None;
        }

        Some(Self {
            mode: LocaleMode::Default,
            config: config.clone(),
        })
    }

    /// A single-locale context for `locale` with fallback off: a read through it
    /// takes exactly the values and rows `locale` holds, never the default
    /// locale's standing in for an empty one — what a snapshot records and a
    /// restore writes back.
    ///
    /// `locale` must be one of `config.locales`: [`Self::access_locale`] answers
    /// the default locale for any other, so an unconfigured one would silently
    /// read and write the default locale's values under its name.
    #[must_use]
    pub fn exact(config: &LocaleConfig, locale: &str) -> Self {
        debug_assert!(
            config.locales.iter().any(|configured| configured == locale),
            "exact locale context for unconfigured locale '{locale}'; configured: {:?}",
            config.locales
        );

        Self {
            mode: LocaleMode::Single(locale.to_string()),
            config: LocaleConfig {
                fallback: false,
                ..config.clone()
            },
        }
    }

    /// Build a `LocaleContext` from an optional locale string and config.
    /// Returns `Ok(None)` if localization is disabled (empty `locales` vec).
    /// `"all"` → `All`, a specific code → `Single`, `None` → `Default`.
    ///
    /// # Errors
    ///
    /// Returns an error if `locale` is not one of the configured locales.
    pub fn from_locale_string(locale: Option<&str>, config: &LocaleConfig) -> Result<Option<Self>> {
        if !config.is_enabled() {
            return Ok(None);
        }

        let mode = match locale {
            Some("all") => LocaleMode::All,
            Some(l) => {
                if !config.locales.iter().any(|loc| loc == l) {
                    bail!(
                        "Invalid locale '{}'. Available locales: {}",
                        l,
                        config.locales.join(", ")
                    );
                }
                LocaleMode::Single(l.to_string())
            }
            None => LocaleMode::Default,
        };
        Ok(Some(Self {
            mode,
            config: config.clone(),
        }))
    }

    /// The single locale this operation targets: the requested locale when it
    /// is configured, else the default locale — also for `Default` / `All`. The
    /// one decision every read and write of a localized value shares: the
    /// select, the write column, filters, join rows, a draft's per-locale keys,
    /// and access / policy decisions. An `Option<&LocaleContext>` is `None` when
    /// localization is disabled, so `locale_ctx.map(LocaleContext::access_locale)`
    /// is `None` exactly when there's no meaningful locale.
    #[must_use]
    pub fn access_locale(&self) -> &str {
        match &self.mode {
            LocaleMode::Single(l) if self.config.locales.contains(l) => l.as_str(),
            _ => self.config.default_locale.as_str(),
        }
    }

    /// The locale a single-locale read takes a localized column from, with the
    /// locale it falls back to while that holds nothing; `None` for an
    /// all-locales read, which takes every locale.
    #[must_use]
    pub(crate) fn read_locale(&self) -> Option<ReadLocale<'_>> {
        if matches!(self.mode, LocaleMode::All) {
            return None;
        }

        Some(self.rows_read_locale())
    }

    /// The locale a localized join field's rows are read in, with its fallback —
    /// an all-locales read takes the default locale's rows.
    #[must_use]
    pub(crate) fn rows_read_locale(&self) -> ReadLocale<'_> {
        let locale = self.access_locale();
        let default = self.config.default_locale.as_str();
        let fallback = (self.config.fallback && locale != default).then_some(default);

        ReadLocale::new(locale, fallback)
    }

    /// The locale to expose to lifecycle hooks as `ctx.locale`. Unlike
    /// [`Self::access_locale`], this returns `None` in `All` mode: an
    /// all-locales read shapes localized fields as a `{ en = .., de = .. }`
    /// map, so claiming a single `ctx.locale` (the default) would mislead a
    /// field hook into overwriting the whole map believing it holds one
    /// locale's scalar. `Single`/`Default` reads target one locale and
    /// expose it normally.
    #[must_use]
    pub fn hook_locale(&self) -> Option<&str> {
        (!matches!(self.mode, LocaleMode::All)).then(|| self.access_locale())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::test_helpers::make_locale_config;

    /// An exact context reads only the requested locale, with fallback off even
    /// when the configuration enables it, and keeps the rest of the configuration.
    #[test]
    fn an_exact_context_reads_one_locale_without_fallback() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };

        let ctx = LocaleContext::exact(&config, "de");

        assert!(matches!(&ctx.mode, LocaleMode::Single(l) if l == "de"));
        assert!(!ctx.config.fallback);
        assert_eq!(ctx.config.default_locale, "en");
        assert_eq!(ctx.config.locales, config.locales);
        assert_eq!(ctx.read_locale(), Some(ReadLocale::new("de", None)));
    }

    /// Regression: the select, join hydration, filters and the draft read each
    /// decided a read's locale on their own, so a locale that isn't configured
    /// read the default locale's columns but its own — empty — rows and draft
    /// values.
    #[test]
    fn a_locale_that_is_not_configured_reads_the_default_locale() {
        let ctx = LocaleContext {
            mode: LocaleMode::Single("fr".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        };

        assert_eq!(ctx.access_locale(), "en");
        assert_eq!(ctx.read_locale(), Some(ReadLocale::new("en", None)));
        assert_eq!(ctx.rows_read_locale(), ReadLocale::new("en", None));
    }

    #[test]
    fn a_configured_locale_falls_back_to_the_default() {
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        };

        assert_eq!(ctx.read_locale(), Some(ReadLocale::new("de", Some("en"))));

        let all = LocaleContext {
            mode: LocaleMode::All,
            ..ctx
        };
        assert_eq!(all.read_locale(), None);
        assert_eq!(all.rows_read_locale(), ReadLocale::new("en", None));
    }

    /// The read expression is the fallback `COALESCE` while a fallback locale
    /// is in play, and the plain quoted column otherwise.
    #[test]
    fn the_read_expression_carries_the_fallback() {
        let with_fallback = ReadLocale::new("de", Some("en"));
        assert_eq!(
            with_fallback.column_expr("title").unwrap(),
            "COALESCE(\"title__de\", \"title__en\")"
        );

        let without = ReadLocale::new("en", None);
        assert_eq!(without.column_expr("title").unwrap(), "\"title__en\"");
    }

    /// A locale code with a hyphen or capitals reaches SQL in its column form,
    /// quoted so Postgres does not fold the case away.
    #[test]
    fn the_read_expression_quotes_a_hyphenated_locale_column() {
        let read = ReadLocale::new("pt-BR", Some("en"));

        assert_eq!(
            read.column_expr("seo__title").unwrap(),
            "COALESCE(\"seo__title__pt_BR\", \"seo__title__en\")"
        );
    }

    #[test]
    fn locale_context_disabled() {
        let config = LocaleConfig::default();
        let ctx = LocaleContext::from_locale_string(None, &config).unwrap();
        assert!(
            ctx.is_none(),
            "Should be None when localization is disabled"
        );
    }

    #[test]
    fn locale_context_all() {
        let config = make_locale_config();
        let ctx = LocaleContext::from_locale_string(Some("all"), &config).unwrap();
        assert!(ctx.is_some());
        assert!(matches!(ctx.unwrap().mode, LocaleMode::All));
    }

    #[test]
    fn locale_context_specific() {
        let config = make_locale_config();
        let ctx = LocaleContext::from_locale_string(Some("de"), &config).unwrap();
        assert!(ctx.is_some());
        match ctx.unwrap().mode {
            LocaleMode::Single(locale) => assert_eq!(locale, "de"),
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn locale_context_nonexistent_locale_returns_error() {
        let config = make_locale_config();
        let result = LocaleContext::from_locale_string(Some("fr"), &config);
        assert!(result.is_err(), "Non-existent locale should return Err");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Invalid locale 'fr'"),
            "Error should mention the invalid locale, got: {err}"
        );
    }

    #[test]
    fn locale_context_default() {
        let config = make_locale_config();
        let ctx = LocaleContext::from_locale_string(None, &config).unwrap();
        assert!(ctx.is_some());
        assert!(matches!(ctx.unwrap().mode, LocaleMode::Default));
    }
}
