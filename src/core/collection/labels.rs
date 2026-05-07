//! Human-readable singular/plural labels and the localized-label resolution helper.

use serde::{Deserialize, Serialize};

use crate::core::LocalizedString;

/// Human-readable singular/plural labels for the admin UI.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Labels {
    /// Singular label for the collection (e.g., "Post").
    #[serde(default)]
    pub singular: Option<LocalizedString>,
    /// Plural label for the collection (e.g., "Posts").
    #[serde(default)]
    pub plural: Option<LocalizedString>,
}

impl Labels {
    /// Create a new labels configuration with singular and plural forms.
    pub fn new(singular: Option<LocalizedString>, plural: Option<LocalizedString>) -> Self {
        Self { singular, plural }
    }
}

/// Resolve a localized label down to a `&str`, falling back to `fallback`
/// when the label is missing or resolves empty. `locale` selects between
/// default-resolution (`None`) and locale-aware resolution (`Some((locale,
/// default_locale))`). Shared by `CollectionDefinition` /
/// `GlobalDefinition` `display_name*` / `singular_name*` methods.
pub(crate) fn resolve_label<'a>(
    label: Option<&'a LocalizedString>,
    fallback: &'a str,
    locale: Option<(&str, &str)>,
) -> &'a str {
    label
        .map(|ls| match locale {
            Some((l, d)) => ls.resolve(l, d),
            None => ls.resolve_default(),
        })
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn localized(pairs: &[(&str, &str)]) -> LocalizedString {
        let mut map = HashMap::new();
        for (k, v) in pairs {
            map.insert((*k).to_string(), (*v).to_string());
        }

        LocalizedString::Localized(map)
    }

    #[test]
    fn falls_back_when_label_is_none() {
        assert_eq!(resolve_label(None, "posts", None), "posts");
        assert_eq!(resolve_label(None, "posts", Some(("de", "en"))), "posts");
    }

    #[test]
    fn falls_back_when_label_resolves_empty() {
        // Empty Localized map → both resolution paths yield ""
        let ls = LocalizedString::Localized(HashMap::new());
        assert_eq!(resolve_label(Some(&ls), "fallback", None), "fallback");
        assert_eq!(
            resolve_label(Some(&ls), "fallback", Some(("de", "en"))),
            "fallback"
        );
    }

    #[test]
    fn resolves_via_default_when_locale_is_none() {
        let ls = LocalizedString::Plain("Plain".into());
        assert_eq!(resolve_label(Some(&ls), "fallback", None), "Plain");

        // Localized + None → resolve_default picks alphabetically-first key
        let ls = localized(&[("de", "Titel"), ("en", "Title")]);
        assert_eq!(resolve_label(Some(&ls), "fallback", None), "Titel");
    }

    #[test]
    fn resolves_via_locale_when_provided() {
        let ls = localized(&[("de", "Titel"), ("en", "Title")]);
        assert_eq!(
            resolve_label(Some(&ls), "fallback", Some(("en", "de"))),
            "Title"
        );
        // Locale missing → falls back to default_locale value
        assert_eq!(
            resolve_label(Some(&ls), "fallback", Some(("fr", "en"))),
            "Title"
        );
    }

    #[test]
    fn plain_label_ignores_locale_args() {
        let ls = LocalizedString::Plain("Always".into());
        assert_eq!(
            resolve_label(Some(&ls), "fallback", Some(("xx", "yy"))),
            "Always"
        );
    }
}
