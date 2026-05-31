//! Editor-locale template data — distinct from [`EditorLocaleContext`].
//!
//! [`LocaleTemplateData`] writes `has_locales` / `current_locale` / `locales`
//! to the template context. [`EditorLocaleContext`] writes the parallel keys
//! `has_editor_locales` / `editor_locale` / `editor_locales`. The two sets
//! coexist for historical reasons (different parts of the admin UI grew
//! against different key names) and both are populated on edit-form pages.

use schemars::JsonSchema;
use serde::Serialize;

use crate::config::LocaleConfig;

/// Per-locale option in the template-data picker.
#[derive(Serialize, JsonSchema)]
pub struct LocaleTemplateOption {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

/// Template-data flavor of the editor locale picker. Flattened into pages
/// that need it (collection edit/create, globals edit).
#[derive(Serialize, JsonSchema)]
pub struct LocaleTemplateData {
    pub has_locales: bool,
    pub current_locale: String,
    pub locales: Vec<LocaleTemplateOption>,
}

impl LocaleTemplateData {
    /// Build template data for the requested locale, or `None` when locale
    /// support is disabled.
    pub fn for_locale(config: &LocaleConfig, requested: Option<&str>) -> Option<Self> {
        if !config.is_enabled() {
            return None;
        }

        let current = requested.unwrap_or(&config.default_locale).to_string();
        let locales: Vec<LocaleTemplateOption> = config
            .locales
            .iter()
            .map(|l| LocaleTemplateOption {
                value: l.clone(),
                label: l.to_uppercase(),
                selected: *l == current,
            })
            .collect();

        Some(Self {
            has_locales: true,
            current_locale: current,
            locales,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn disabled_config_contributes_nothing() {
        assert!(LocaleTemplateData::for_locale(&LocaleConfig::default(), Some("en")).is_none());
    }

    #[test]
    fn marks_requested_locale_selected_and_uppercases_labels() {
        let d = LocaleTemplateData::for_locale(&enabled(), Some("de")).unwrap();
        assert!(d.has_locales);
        assert_eq!(d.current_locale, "de");
        assert_eq!(
            (
                d.locales[0].value.as_str(),
                d.locales[0].label.as_str(),
                d.locales[0].selected
            ),
            ("en", "EN", false)
        );
        assert!(d.locales[1].selected); // de
    }

    #[test]
    fn falls_back_to_default_locale_when_none_requested() {
        let d = LocaleTemplateData::for_locale(&enabled(), None).unwrap();
        assert_eq!(d.current_locale, "en");
        assert!(d.locales[0].selected);
    }
}
