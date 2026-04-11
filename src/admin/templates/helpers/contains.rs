use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::Value;

/// String/array contains helper: `{{#if (contains arr val)}}`.
pub(super) struct ContainsHelper;

impl HelperDef for ContainsHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let haystack = h.param(0).map(|p| p.value()).unwrap_or(&Value::Null);
        let needle = h.param(1).map(|p| p.value()).unwrap_or(&Value::Null);
        let result = match haystack {
            Value::String(s) => needle.as_str().map(|n| s.contains(n)).unwrap_or(false),
            Value::Array(arr) => arr.contains(needle),
            _ => false,
        };
        Ok(ScopedJson::Derived(Value::Bool(result)))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::admin::templates::helpers::test_helpers::test_hbs;

    #[test]
    fn contains_string() {
        let mut hbs = test_hbs();
        hbs.register_template_string(
            "t",
            "{{#if (contains haystack needle)}}YES{{else}}NO{{/if}}",
        )
        .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"haystack": "hello world", "needle": "world"}))
                .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &json!({"haystack": "hello world", "needle": "xyz"}))
                .unwrap(),
            "NO"
        );
    }

    #[test]
    fn contains_array() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (contains arr val)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"arr": ["a", "b", "c"], "val": "b"}))
                .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &json!({"arr": ["a", "b", "c"], "val": "d"}))
                .unwrap(),
            "NO"
        );
    }

    #[test]
    fn non_string_non_array_returns_false() {
        let mut hbs = test_hbs();
        hbs.register_template_string(
            "t",
            "{{#if (contains haystack needle)}}YES{{else}}NO{{/if}}",
        )
        .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"haystack": 42, "needle": "4"}))
                .unwrap(),
            "NO"
        );
        assert_eq!(
            hbs.render("t", &json!({"haystack": true, "needle": "t"}))
                .unwrap(),
            "NO"
        );
        assert_eq!(
            hbs.render("t", &json!({"haystack": null, "needle": "x"}))
                .unwrap(),
            "NO"
        );
        assert_eq!(
            hbs.render("t", &json!({"haystack": {"a": 1}, "needle": "a"}))
                .unwrap(),
            "NO"
        );
    }

    #[test]
    fn string_with_non_string_needle() {
        let mut hbs = test_hbs();
        hbs.register_template_string(
            "t",
            "{{#if (contains haystack needle)}}YES{{else}}NO{{/if}}",
        )
        .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"haystack": "hello 42 world", "needle": 42}))
                .unwrap(),
            "NO"
        );
    }

    #[test]
    fn array_with_number_needle() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{#if (contains arr val)}}YES{{else}}NO{{/if}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"arr": [1, 2, 3], "val": 2}))
                .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &json!({"arr": [1, 2, 3], "val": 4}))
                .unwrap(),
            "NO"
        );
    }
}
