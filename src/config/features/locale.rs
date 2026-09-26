//! Locale / i18n configuration.

use std::collections::HashMap;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{config::ErrorReport, db::query::sanitize_locale};

/// Internationalization / locale configuration.
#[derive(Debug, Clone, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
#[serde(default, deny_unknown_fields)]
pub struct LocaleConfig {
    /// Default locale code. Content without explicit locale uses this.
    pub default_locale: String,
    /// All supported locale codes. Empty = localization disabled.
    pub locales: Vec<String>,
    /// When true, reading a locale falls back to `default_locale` if the field is NULL.
    pub fallback: bool,
}

impl Default for LocaleConfig {
    fn default() -> Self {
        Self {
            default_locale: "en".to_string(),
            locales: Vec::new(),
            fallback: true,
        }
    }
}

impl LocaleConfig {
    /// Returns true if localization is enabled (at least one locale defined).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.locales.is_empty()
    }

    /// A stable fingerprint of the parts of the configuration the stored
    /// columns depend on: the default locale and the SET of locales.
    ///
    /// Versioned one-time migrations store it alongside their version so a
    /// locale added or removed — which changes which columns exist and which
    /// ones a computation walks — makes the gated work run again instead of
    /// leaving a stale result behind forever.
    ///
    /// The codes are sorted, because the order they are listed in changes no
    /// column and no walk: reordering `locales` would otherwise reopen every
    /// gate and recompute a whole database's worth of work for nothing. The
    /// default locale leads the value — it decides which column a bare read
    /// takes — so [`Self::default_locale_of_fingerprint`] can still read it
    /// back.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut codes: Vec<&str> = self.locales.iter().map(String::as_str).collect();
        codes.sort_unstable();

        format!("{}|{}", self.default_locale, codes.join(","))
    }

    /// The default locale recorded in a [`Self::fingerprint`] value, or `None`
    /// when the value has no fingerprint shape. Locale codes contain no `|`,
    /// so the split is unambiguous.
    #[must_use]
    pub fn default_locale_of_fingerprint(fingerprint: &str) -> Option<&str> {
        fingerprint.split_once('|').map(|(default, _)| default)
    }

    /// Validate that all locale codes are safe identifiers (alphanumeric, hyphens,
    /// underscores only). This prevents SQL injection via locale strings that are
    /// interpolated into DDL during migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if any locale code contains disallowed characters.
    pub fn validate(&self) -> Result<()> {
        let mut report = ErrorReport::new();
        self.collect_problems(&mut report);

        report.into_result()
    }

    /// Record every problem [`validate`](Self::validate) finds: each invalid
    /// code, then — once every code is valid — colliding locales and a
    /// default locale missing from the list.
    pub(crate) fn collect_problems(&self, report: &mut ErrorReport) {
        // The default locale is usually listed too; each code is judged once.
        let default_unlisted =
            (!self.locales.contains(&self.default_locale)).then_some(&self.default_locale);
        let codes = default_unlisted.into_iter().chain(&self.locales);
        let mut valid = true;

        for code in codes {
            valid &= report.check(Self::validate_locale_code(code)).is_some();
        }

        if !valid {
            return;
        }

        report.check(self.reject_colliding_locales());

        // When locales are enabled, the default locale must be in the list
        if !self.locales.is_empty() && !self.locales.contains(&self.default_locale) {
            report.push_message(format!(
                "default_locale '{}' must be included in the locales list {:?}",
                self.default_locale, self.locales
            ));
        }
    }

    /// Reject two locales that would share a column.
    ///
    /// Columns are named after the locale's SANITIZED form, so `pt-BR` and
    /// `pt_BR` both store into `title__pt_BR`: the migration either fails with
    /// a confusing duplicate-column error or, where the column already exists,
    /// two locales silently overwrite each other's content. Comparing the raw
    /// codes missed that entirely.
    ///
    /// The comparison is also case-insensitive. `SQLite` treats `title__pt_BR`
    /// and `title__pt_br` as one column (the first sync fails with a raw
    /// duplicate-column error) while Postgres creates two, so `pt-BR` and
    /// `pt-br` are refused up front, the same way on both backends.
    fn reject_colliding_locales(&self) -> Result<()> {
        let mut by_column: HashMap<String, (&str, String)> = HashMap::new();

        for locale in &self.locales {
            let column_form = sanitize_locale(locale)?;
            let column_key = column_form.to_ascii_lowercase();

            let Some((previous, previous_column)) =
                by_column.insert(column_key, (locale.as_str(), column_form))
            else {
                continue;
            };

            if previous == locale.as_str() {
                bail!("Duplicate locale '{locale}' in the locales list");
            }

            bail!(
                "Locales '{previous}' and '{locale}' both store their values in \
                 '__{previous_column}' columns (column names are case-insensitive) — \
                 locale codes must differ by more than a separator or letter case"
            );
        }

        Ok(())
    }

    fn validate_locale_code(code: &str) -> Result<()> {
        if code.is_empty() {
            bail!("Locale code must not be empty");
        }

        if !code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!(
                "Invalid locale code '{code}': only ASCII alphanumeric, hyphens, and underscores allowed"
            );
        }

        // A code of only separators (e.g. "---") would produce a degenerate
        // SQL column name like `field____`.
        if !code.chars().any(|c| c.is_ascii_alphanumeric()) {
            bail!("Invalid locale code '{code}': must contain at least one alphanumeric character");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_config_is_enabled() {
        let empty = LocaleConfig::default();
        assert!(!empty.is_enabled(), "empty locales should be disabled");

        let with_locales = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        assert!(
            with_locales.is_enabled(),
            "non-empty locales should be enabled"
        );
    }

    #[test]
    fn locale_validation_rejects_duplicates() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string(), "en".to_string()],
            fallback: true,
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("Duplicate locale 'en'"), "unexpected: {err}");
    }

    /// Column names use the locale's sanitized form, so `pt-BR` and `pt_BR`
    /// are the same column. Validating the raw codes let the pair through and
    /// the migration failed (or two locales shared one column's content).
    #[test]
    fn locale_validation_rejects_codes_that_share_a_column() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "pt-BR".to_string(), "pt_BR".to_string()],
            fallback: true,
        };

        let err = config.validate().unwrap_err().to_string();

        assert!(err.contains("'pt-BR' and 'pt_BR'"), "unexpected: {err}");
        assert!(err.contains("__pt_BR"), "unexpected: {err}");
    }

    /// Regression: `pt-BR` and `pt-br` passed validation (their column forms
    /// differ in case), then `SQLite` refused the second column as a duplicate
    /// while Postgres created both. One rule for both backends: locale codes
    /// that differ only in letter case are refused at config load.
    #[test]
    fn locale_validation_rejects_codes_that_differ_only_in_case() {
        for (first, second) in [("pt-BR", "pt-br"), ("en", "EN")] {
            let config = LocaleConfig {
                default_locale: first.to_string(),
                locales: vec![first.to_string(), second.to_string()],
                fallback: true,
            };

            let err = config.validate().unwrap_err().to_string();

            assert!(
                err.contains(&format!("'{first}' and '{second}'")),
                "unexpected: {err}"
            );
            assert!(err.contains("case-insensitive"), "unexpected: {err}");
        }
    }

    /// The fingerprint follows the default locale and the set of locales —
    /// what the stored columns and the computations over them depend on.
    #[test]
    fn the_fingerprint_tracks_the_default_locale_and_the_locale_list() {
        let en_de = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };

        assert_eq!(en_de.fingerprint(), "en|de,en");
        assert_eq!(
            LocaleConfig::default_locale_of_fingerprint(&en_de.fingerprint()),
            Some("en")
        );

        let dropped = LocaleConfig {
            locales: vec!["en".to_string()],
            ..en_de.clone()
        };
        let moved_default = LocaleConfig {
            default_locale: "de".to_string(),
            ..en_de.clone()
        };

        for other in [dropped, moved_default] {
            assert_ne!(en_de.fingerprint(), other.fingerprint());
        }

        assert_eq!(
            LocaleConfig::default_locale_of_fingerprint("no-separator"),
            None
        );
    }

    /// Listing the same locales in another order changes no column and no
    /// walk, so the fingerprint must not move — a gate keyed on it would
    /// otherwise recompute a whole database's worth of work for nothing.
    #[test]
    fn reordering_the_locales_leaves_the_fingerprint_alone() {
        let en_de = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let reordered = LocaleConfig {
            locales: vec!["de".to_string(), "en".to_string()],
            ..en_de.clone()
        };

        assert_eq!(en_de.fingerprint(), reordered.fingerprint());
        assert_eq!(
            LocaleConfig::default_locale_of_fingerprint(&reordered.fingerprint()),
            Some("en"),
            "the default locale still leads the value"
        );
    }

    #[test]
    fn locale_validation_rejects_separator_only_code() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "---".to_string()],
            fallback: true,
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("at least one alphanumeric"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn locale_validation_valid_codes() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec![
                "en".to_string(),
                "de".to_string(),
                "pt-BR".to_string(),
                "zh_CN".to_string(),
            ],
            fallback: true,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn locale_validation_rejects_sql_injection() {
        let config = LocaleConfig {
            default_locale: "en'; DROP TABLE posts; --".to_string(),
            locales: vec![],
            fallback: true,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn locale_validation_rejects_empty() {
        let config = LocaleConfig {
            default_locale: String::new(),
            locales: vec![],
            fallback: true,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn locale_validation_rejects_bad_locale_in_list() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de/../etc".to_string()],
            fallback: true,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn locale_validation_default_not_in_list_errors() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["de".to_string(), "fr".to_string()],
            fallback: true,
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_locale"));
    }

    #[test]
    fn locale_validation_default_in_list_passes() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn locale_validation_empty_locales_skips_inclusion_check() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec![],
            fallback: true,
        };
        assert!(config.validate().is_ok());
    }

    /// Regression: validation stopped at the first bad locale code. Every
    /// invalid code is reported.
    #[test]
    fn every_invalid_locale_code_is_reported() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "d e".to_string(), "f;r".to_string()],
            fallback: true,
        };

        let err = config.validate().unwrap_err().to_string();

        assert!(err.starts_with("2 problems:"), "{err}");
        assert!(err.contains("d e") && err.contains("f;r"), "{err}");
    }
}
