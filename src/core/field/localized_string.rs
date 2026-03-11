//! A string that can be plain or per-locale.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Plain: `"Title"` — works like before.
/// Localized: `{ en = "Title", de = "Titel" }` — resolved at render time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LocalizedString {
    /// A simple, non-localized string.
    Plain(String),
    /// A map of locale identifiers to their localized strings.
    Localized(HashMap<String, String>),
}

impl LocalizedString {
    /// Resolve to a single string for the given locale, with fallback to default locale.
    pub fn resolve(&self, locale: &str, default_locale: &str) -> &str {
        match self {
            LocalizedString::Plain(s) => s,
            LocalizedString::Localized(map) => map
                .get(locale)
                .or_else(|| map.get(default_locale))
                .map(|s| s.as_str())
                .unwrap_or(""),
        }
    }

    /// Resolve using the default locale only (for when locale config is disabled).
    pub fn resolve_default(&self) -> &str {
        match self {
            LocalizedString::Plain(s) => s,
            LocalizedString::Localized(map) => {
                map.values().next().map(|s| s.as_str()).unwrap_or("")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localized_string_resolve_existing_locale() {
        let mut map = HashMap::new();
        map.insert("en".to_string(), "Title".to_string());
        map.insert("de".to_string(), "Titel".to_string());
        let ls = LocalizedString::Localized(map);
        assert_eq!(ls.resolve("de", "en"), "Titel");
    }

    #[test]
    fn localized_string_resolve_fallback_to_default() {
        let mut map = HashMap::new();
        map.insert("en".to_string(), "Title".to_string());
        let ls = LocalizedString::Localized(map);
        assert_eq!(ls.resolve("fr", "en"), "Title");
    }

    #[test]
    fn localized_string_resolve_default_plain() {
        let ls = LocalizedString::Plain("Hello".to_string());
        assert_eq!(ls.resolve_default(), "Hello");
        assert_eq!(ls.resolve("de", "en"), "Hello");
    }

    #[test]
    fn localized_string_resolve_default_empty() {
        let map = HashMap::new();
        let ls = LocalizedString::Localized(map);
        assert_eq!(ls.resolve("en", "en"), "");
        assert_eq!(ls.resolve_default(), "");
    }
}
