//! Editor (content) locale context — distinct from UI translation locale.
//!
//! Top-level keys: `has_editor_locales` (bool), `editor_locale` (string),
//! `editor_locales` (Vec<EditorLocaleOption>). Templates use the array to
//! render a locale picker and the string to mark the active one.

use schemars::JsonSchema;
use serde::Serialize;

use crate::{config::LocaleConfig, typegen::LuaAnnotation};

/// Top-level context contributed by the editor-locale builder.
#[derive(Serialize, JsonSchema)]
pub struct EditorLocaleContext {
    pub has_editor_locales: bool,
    pub editor_locale: String,
    pub editor_locales: Vec<EditorLocaleOption>,
}

/// One row in the editor-locale picker dropdown.
#[derive(Serialize, JsonSchema, LuaAnnotation)]
#[lua(class = "crap.template.editor_locale_option")]
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
        assert!(EditorLocaleContext::for_locale(Some("en"), &LocaleConfig::default()).is_none());
    }

    #[test]
    fn marks_the_current_locale_selected_and_uppercases_labels() {
        let ctx = EditorLocaleContext::for_locale(Some("de"), &enabled()).unwrap();
        assert!(ctx.has_editor_locales);
        assert_eq!(ctx.editor_locale, "de");
        assert_eq!(ctx.editor_locales.len(), 2);

        let en = &ctx.editor_locales[0];
        assert_eq!(
            (en.value.as_str(), en.label.as_str(), en.selected),
            ("en", "EN", false)
        );
        let de = &ctx.editor_locales[1];
        assert_eq!(
            (de.value.as_str(), de.label.as_str(), de.selected),
            ("de", "DE", true)
        );
    }

    #[test]
    fn falls_back_to_default_locale_when_none_given() {
        let ctx = EditorLocaleContext::for_locale(None, &enabled()).unwrap();
        assert_eq!(ctx.editor_locale, "en");
        assert!(ctx.editor_locales[0].selected); // en is default → selected
    }
}
