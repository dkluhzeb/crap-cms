//! Locale / i18n configuration.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Internationalization / locale configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
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

    /// Validate that all locale codes are safe identifiers (alphanumeric, hyphens,
    /// underscores only). This prevents SQL injection via locale strings that are
    /// interpolated into DDL during migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if any locale code contains disallowed characters.
    pub fn validate(&self) -> Result<()> {
        Self::validate_locale_code(&self.default_locale)?;

        for locale in &self.locales {
            Self::validate_locale_code(locale)?;
        }

        // When locales are enabled, the default locale must be in the list
        if !self.locales.is_empty() && !self.locales.contains(&self.default_locale) {
            bail!(
                "default_locale '{}' must be included in the locales list {:?}",
                self.default_locale,
                self.locales
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
}
