use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::Value;

use super::is_truthy;

/// Boolean negation helper: `{{#if (not val)}}`.
pub(super) struct NotHelper;

impl HelperDef for NotHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let val = h.param(0).map(|p| p.value()).unwrap_or(&Value::Null);
        let truthy = is_truthy(val);
        Ok(ScopedJson::Derived(Value::Bool(!truthy)))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    fn test_hbs() -> handlebars::Handlebars<'static> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations =
            std::sync::Arc::new(crate::admin::translations::Translations::load(tmp.path()));
        let hbs = crate::admin::templates::create_handlebars(tmp.path(), false, translations)
            .expect("create_handlebars");
        (*hbs).clone()
    }

    #[test]
    fn not_bool() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (not val)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"val": false})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"val": true})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &json!({"val": null})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"val": "hello"})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &json!({"val": ""})).unwrap(), "YES");
    }

    #[test]
    fn not_number() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (not val)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"val": 0})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"val": 1})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &json!({"val": -1})).unwrap(), "NO");
    }

    #[test]
    fn not_array() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (not val)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"val": []})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"val": [1]})).unwrap(), "NO");
    }

    #[test]
    fn not_object() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (not val)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"val": {}})).unwrap(), "NO");
    }
}
