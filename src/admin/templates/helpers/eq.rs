use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::Value;

/// Handlebars helper for equality comparison.
pub(super) struct EqHelper;

impl HelperDef for EqHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let a = h.param(0).map(|p| p.value());
        let b = h.param(1).map(|p| p.value());
        let result = a == b;
        Ok(ScopedJson::Derived(Value::Bool(result)))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::admin::templates::helpers::test_helpers::test_hbs;

    #[test]
    fn eq_strings() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (eq a b)}}EQUAL{{else}}NOT_EQUAL{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": "foo", "b": "foo"})).unwrap(),
            "EQUAL"
        );
        assert_eq!(
            hbs.render("t", &json!({"a": "foo", "b": "bar"})).unwrap(),
            "NOT_EQUAL"
        );
    }

    #[test]
    fn eq_numbers() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (eq a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"a": 42, "b": 42})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"a": 42, "b": 43})).unwrap(), "NO");
    }

    #[test]
    fn eq_null() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (eq a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": null, "b": null})).unwrap(),
            "YES"
        );
    }

    #[test]
    fn eq_bool() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (eq a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": true, "b": true})).unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &json!({"a": true, "b": false})).unwrap(),
            "NO"
        );
    }

    #[test]
    fn eq_type_mismatch() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (eq a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"a": "42", "b": 42})).unwrap(), "NO");
    }
}
