use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::Value;

use super::is_truthy;

/// Logical AND helper: `{{#if (and a b)}}`.
pub(super) struct AndHelper;

impl HelperDef for AndHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let a = h.param(0).map(|p| p.value()).unwrap_or(&Value::Null);
        let b = h.param(1).map(|p| p.value()).unwrap_or(&Value::Null);
        Ok(ScopedJson::Derived(Value::Bool(
            is_truthy(a) && is_truthy(b),
        )))
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
    fn and_bools() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (and a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": true, "b": true})).unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &json!({"a": true, "b": false})).unwrap(),
            "NO"
        );
        assert_eq!(
            hbs.render("t", &json!({"a": false, "b": true})).unwrap(),
            "NO"
        );
    }

    #[test]
    fn and_strings() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (and a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": "hello", "b": "world"}))
                .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &json!({"a": "hello", "b": ""})).unwrap(),
            "NO"
        );
    }

    #[test]
    fn and_numbers() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (and a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"a": 1, "b": 2})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"a": 1, "b": 0})).unwrap(), "NO");
    }
}
