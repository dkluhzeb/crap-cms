//! Editor-locale template data — distinct from [`EditorLocaleContext`].
//!
//! [`LocaleTemplateData`] writes `has_locales` / `current_locale` / `locales`
//! to the template context. [`EditorLocaleContext`] writes the parallel keys
//! `has_editor_locales` / `editor_locale` / `editor_locales`. The two sets
//! coexist for historical reasons (different parts of the admin UI grew
//! against different key names) and both are populated on edit-form pages.

use serde::Serialize;

use crate::config::LocaleConfig;

/// Per-locale option in the template-data picker.
#[derive(Serialize)]
pub struct LocaleTemplateOption {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

/// Template-data flavor of the editor locale picker. Flattened into pages
/// that need it (collection edit/create, globals edit).
#[derive(Serialize)]
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
