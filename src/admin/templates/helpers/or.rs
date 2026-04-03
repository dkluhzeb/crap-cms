use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::Value;

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
        let a = h.param(0).map(|p| p.value()).unwrap_or(&Value::Null);
        let b = h.param(1).map(|p| p.value()).unwrap_or(&Value::Null);
        Ok(ScopedJson::Derived(Value::Bool(
            is_truthy(a) || is_truthy(b),
        )))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::admin::templates::helpers::test_helpers::test_hbs;

    #[test]
    fn or_bools() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (or a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": true, "b": false})).unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &json!({"a": false, "b": true})).unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &json!({"a": false, "b": false})).unwrap(),
            "NO"
        );
    }

    #[test]
    fn or_strings() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (or a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"a": "", "b": ""})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &json!({"a": "", "b": "x"})).unwrap(), "YES");
    }

    #[test]
    fn or_null_and_value() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (or a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": null, "b": null})).unwrap(),
            "NO"
        );
        assert_eq!(hbs.render("t", &json!({"a": null, "b": 1})).unwrap(), "YES");
    }
}
