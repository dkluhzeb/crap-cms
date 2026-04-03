use std::{collections::HashMap, sync::Arc};

use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::Value;

use crate::admin::Translations;

/// Handlebars helper for admin UI translations.
/// Usage: `{{t "key"}}` or with interpolation: `{{t "key" name=value}}`
/// Interpolation replaces `{{var}}` placeholders in the translation string.
#[allow(dead_code)]
pub(super) struct TranslationHelper {
    pub(super) translations: Arc<Translations>,
}

impl HelperDef for TranslationHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let key = h.param(0).and_then(|p| p.value().as_str()).unwrap_or("");

        // Read locale from template context (_locale), default to "en"
        let locale = ctx
            .data()
            .get("_locale")
            .and_then(|v| v.as_str())
            .unwrap_or("en");

        let hash = h.hash();
        if hash.is_empty() {
            let translated = self.translations.get(locale, key);

            Ok(ScopedJson::Derived(Value::String(translated.to_string())))
        } else {
            let mut params = HashMap::new();
            for (k, v) in hash {
                let val = match v.value() {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    other => other.to_string(),
                };

                params.insert(k.to_string(), val);
            }

            let translated = self.translations.get_interpolated(locale, key, &params);

            Ok(ScopedJson::Derived(Value::String(translated)))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    fn test_hbs_with_translations(config_dir: &std::path::Path) -> handlebars::Handlebars<'static> {
        let translations =
            std::sync::Arc::new(crate::admin::translations::Translations::load(config_dir));
        let hbs = crate::admin::templates::create_handlebars(config_dir, false, translations)
            .expect("hbs");
        (*hbs).clone()
    }

    #[test]
    fn simple_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations_dir = tmp.path().join("translations");
        fs::create_dir_all(&translations_dir).unwrap();
        fs::write(
            translations_dir.join("en.json"),
            r#"{"hello": "Hello World"}"#,
        )
        .unwrap();

        let mut hbs = test_hbs_with_translations(tmp.path());
        hbs.register_template_string("t", "{{t \"hello\"}}")
            .unwrap();
        let result = hbs.render("t", &json!({})).unwrap();
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn with_interpolation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations_dir = tmp.path().join("translations");
        fs::create_dir_all(&translations_dir).unwrap();
        fs::write(
            translations_dir.join("en.json"),
            r#"{"greeting": "Hello {{name}}, you have {{count}} items"}"#,
        )
        .unwrap();

        let mut hbs = test_hbs_with_translations(tmp.path());
        hbs.register_template_string("t", "{{t \"greeting\" name=\"Alice\" count=5}}")
            .unwrap();
        let result = hbs.render("t", &json!({})).unwrap();
        assert_eq!(result, "Hello Alice, you have 5 items");
    }

    #[test]
    fn interpolation_with_bool() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations_dir = tmp.path().join("translations");
        fs::create_dir_all(&translations_dir).unwrap();
        fs::write(
            translations_dir.join("en.json"),
            r#"{"status": "Active: {{active}}"}"#,
        )
        .unwrap();

        let mut hbs = test_hbs_with_translations(tmp.path());
        hbs.register_template_string("t", "{{t \"status\" active=true}}")
            .unwrap();
        let result = hbs.render("t", &json!({})).unwrap();
        assert_eq!(result, "Active: true");
    }

    #[test]
    fn missing_key_returns_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut hbs = test_hbs_with_translations(tmp.path());
        hbs.register_template_string("t", "{{t \"nonexistent.key\"}}")
            .unwrap();
        let result = hbs.render("t", &json!({})).unwrap();
        assert_eq!(result, "nonexistent.key");
    }
}
