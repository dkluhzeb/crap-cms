use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};

use super::is_truthy;

/// Logical OR helper: `{{#if (or a b)}}`.
pub(super) struct OrHelper;

impl HelperDef for OrHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let a = h
            .param(0)
            .map(|p| p.value())
            .unwrap_or(&serde_json::Value::Null);
        let b = h
            .param(1)
            .map(|p| p.value())
            .unwrap_or(&serde_json::Value::Null);
        Ok(ScopedJson::Derived(serde_json::Value::Bool(
            is_truthy(a) || is_truthy(b),
        )))
    }
}

#[cfg(test)]
mod tests {
    fn test_hbs() -> handlebars::Handlebars<'static> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations =
            std::sync::Arc::new(crate::admin::translations::Translations::load(tmp.path()));
        let hbs = crate::admin::templates::create_handlebars(tmp.path(), false, translations)
            .expect("create_handlebars");
        (*hbs).clone()
    }

    #[test]
    fn or_bools() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (or a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &serde_json::json!({"a": true, "b": false}))
                .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &serde_json::json!({"a": false, "b": true}))
                .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &serde_json::json!({"a": false, "b": false}))
                .unwrap(),
            "NO"
        );
    }

    #[test]
    fn or_strings() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (or a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &serde_json::json!({"a": "", "b": ""}))
                .unwrap(),
            "NO"
        );
        assert_eq!(
            hbs.render("t", &serde_json::json!({"a": "", "b": "x"}))
                .unwrap(),
            "YES"
        );
    }

    #[test]
    fn or_null_and_value() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (or a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &serde_json::json!({"a": null, "b": null}))
                .unwrap(),
            "NO"
        );
        assert_eq!(
            hbs.render("t", &serde_json::json!({"a": null, "b": 1}))
                .unwrap(),
            "YES"
        );
    }
}
