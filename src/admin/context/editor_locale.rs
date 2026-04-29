//! Editor (content) locale context — distinct from UI translation locale.
//!
//! Top-level keys: `has_editor_locales` (bool), `editor_locale` (string),
//! `editor_locales` (Vec<EditorLocaleOption>). Templates use the array to
//! render a locale picker and the string to mark the active one.

use serde::Serialize;

use crate::config::LocaleConfig;

/// Top-level context contributed by the editor-locale builder.
#[derive(Serialize)]
pub struct EditorLocaleContext {
    pub has_editor_locales: bool,
    pub editor_locale: String,
    pub editor_locales: Vec<EditorLocaleOption>,
}

/// One row in the editor-locale picker dropdown.
#[derive(Serialize)]
pub struct EditorLocaleOption {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

impl EditorLocaleContext {
    /// Build for the given current locale and config. Returns `None` when
    /// content-locale support is not enabled (in which case the builder skips
    /// inserting any of the keys at all).
    pub fn for_locale(editor_locale: Option<&str>, config: &LocaleConfig) -> Option<Self> {
        if !config.is_enabled() {
            return None;
        }

        let current = editor_locale.unwrap_or(&config.default_locale);

        let editor_locales: Vec<EditorLocaleOption> = config
            .locales
            .iter()
            .map(|l| EditorLocaleOption {
                value: l.clone(),
                label: l.to_uppercase(),
                selected: l == current,
            })
            .collect();

        Some(Self {
            has_editor_locales: true,
            editor_locale: current.to_string(),
            editor_locales,
        })
    }
}
