use std::cmp::Ordering;

use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::Value;

use super::as_f64;

/// Numeric comparison helper. Matches if the comparison result is any of the given orderings.
/// - gt:  `CompareHelper(&[Greater])`
/// - lt:  `CompareHelper(&[Less])`
/// - gte: `CompareHelper(&[Greater, Equal])`
/// - lte: `CompareHelper(&[Less, Equal])`
pub(super) struct CompareHelper(pub(super) &'static [Ordering]);

impl HelperDef for CompareHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let a = h.param(0).and_then(|p| as_f64(p.value())).unwrap_or(0.0);
        let b = h.param(1).and_then(|p| as_f64(p.value())).unwrap_or(0.0);
        let result = a
            .partial_cmp(&b)
            .map(|o| self.0.contains(&o))
            .unwrap_or(false);
        Ok(ScopedJson::Derived(Value::Bool(result)))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::admin::templates::helpers::test_helpers::test_hbs;

    #[test]
    fn gt() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (gt a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"a": 10, "b": 5})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"a": 5, "b": 10})).unwrap(), "NO");
        assert_eq!(hbs.render("t", &json!({"a": 5, "b": 5})).unwrap(), "NO");
    }

    #[test]
    fn lt() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (lt a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"a": 3, "b": 7})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"a": 7, "b": 3})).unwrap(), "NO");
    }

    #[test]
    fn gte() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (gte a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"a": 5, "b": 5})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"a": 6, "b": 5})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"a": 4, "b": 5})).unwrap(), "NO");
    }

    #[test]
    fn lte() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (lte a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(hbs.render("t", &json!({"a": 5, "b": 5})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"a": 4, "b": 5})).unwrap(), "YES");
        assert_eq!(hbs.render("t", &json!({"a": 6, "b": 5})).unwrap(), "NO");
    }

    #[test]
    fn string_numbers() {
        let mut hbs = test_hbs();
        hbs.register_template_string("gt_t", "{{#if (gt a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("gt_t", &json!({"a": "10", "b": "5"})).unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("gt_t", &json!({"a": "3", "b": "7"})).unwrap(),
            "NO"
        );

        hbs.register_template_string("lt_t", "{{#if (lt a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("lt_t", &json!({"a": "2.5", "b": "3.5"}))
                .unwrap(),
            "YES"
        );
    }

    #[test]
    fn gte_equal_mixed_types() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (gte a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": "5.0", "b": 5})).unwrap(),
            "YES"
        );
    }

    #[test]
    fn lte_equal_mixed_types() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (lte a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": 5, "b": "5.0"})).unwrap(),
            "YES"
        );
    }

    #[test]
    fn non_numeric_defaults_to_zero() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (gt a b)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": null, "b": null})).unwrap(),
            "NO"
        );
        assert_eq!(hbs.render("t", &json!({"a": 1, "b": null})).unwrap(), "YES");
    }
}
