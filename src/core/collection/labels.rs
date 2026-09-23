//! Human-readable singular/plural labels and the localized-label resolution helper.

use serde::{Deserialize, Serialize};

use crate::core::LocalizedString;
use crate::typegen::lua::LuaAnnotation;

/// Human-readable singular/plural labels for the admin UI.
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[lua(class = "crap.Labels")]
pub struct Labels {
    /// Singular display name (e.g., "Post" or `{ en = "Post", de = "Beitrag" }`).
    #[serde(default)]
    #[lua(ty = "crap.LocalizedString")]
    pub singular: Option<LocalizedString>,
    /// Plural display name (e.g., "Posts" or `{ en = "Posts", de = "Beiträge" }`).
    #[serde(default)]
    #[lua(ty = "crap.LocalizedString")]
    pub plural: Option<LocalizedString>,
}

impl Labels {
    /// Create a new labels configuration with singular and plural forms.
    #[must_use]
    pub fn new(singular: Option<LocalizedString>, plural: Option<LocalizedString>) -> Self {
        Self { singular, plural }
    }
}

/// Resolve a localized label down to a `&str` for the active label locale
/// (see [`LocalizedString::resolve_current`]), falling back to `fallback` when
/// the label is missing or resolves empty. Shared by `CollectionDefinition` /
/// `GlobalDefinition` `display_name` / `singular_name`.
pub(crate) fn resolve_label<'a>(label: Option<&'a LocalizedString>, fallback: &'a str) -> &'a str {
    label
        .map(LocalizedString::resolve_current)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::core::with_label_locale;

    fn localized(pairs: &[(&str, &str)]) -> LocalizedString {
        let mut map = HashMap::new();
        for (k, v) in pairs {
            map.insert((*k).to_string(), (*v).to_string());
        }

        LocalizedString::Localized(map)
    }

    #[test]
    fn falls_back_when_label_is_none() {
        assert_eq!(resolve_label(None, "posts"), "posts");
    }

    #[test]
    fn falls_back_when_label_resolves_empty() {
        let ls = LocalizedString::Localized(HashMap::new());
        assert_eq!(resolve_label(Some(&ls), "fallback"), "fallback");
    }

    #[test]
    fn plain_label_is_used_as_is() {
        let ls = LocalizedString::Plain("Plain".into());
        assert_eq!(resolve_label(Some(&ls), "fallback"), "Plain");
    }

    /// Regression: a localized collection label resolved to the
    /// alphabetically-first key for every viewer. It follows the active label
    /// locale.
    #[tokio::test]
    async fn localized_label_follows_the_label_locale() {
        let ls = localized(&[("de", "Beiträge"), ("en", "Posts")]);

        let en = with_label_locale("en".to_string(), async {
            resolve_label(Some(&ls), "fallback").to_string()
        });
        assert_eq!(en.await, "Posts");

        let de = with_label_locale("de".to_string(), async {
            resolve_label(Some(&ls), "fallback").to_string()
        });
        assert_eq!(de.await, "Beiträge");
    }
}
