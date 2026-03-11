use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};

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
        let haystack = h
            .param(0)
            .map(|p| p.value())
            .unwrap_or(&serde_json::Value::Null);
        let needle = h
            .param(1)
            .map(|p| p.value())
            .unwrap_or(&serde_json::Value::Null);
        let result = match haystack {
            serde_json::Value::String(s) => needle.as_str().map(|n| s.contains(n)).unwrap_or(false),
            serde_json::Value::Array(arr) => arr.contains(needle),
            _ => false,
        };
        Ok(ScopedJson::Derived(serde_json::Value::Bool(result)))
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
    fn contains_string() {
        let mut hbs = test_hbs();
        hbs.register_template_string(
            "t",
            "{{#if (contains haystack needle)}}YES{{else}}NO{{/if}}",
        )
        .unwrap();
        assert_eq!(
            hbs.render(
                "t",
                &serde_json::json!({"haystack": "hello world", "needle": "world"})
            )
            .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render(
                "t",
                &serde_json::json!({"haystack": "hello world", "needle": "xyz"})
            )
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
            hbs.render(
                "t",
                &serde_json::json!({"arr": ["a", "b", "c"], "val": "b"})
            )
            .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render(
                "t",
                &serde_json::json!({"arr": ["a", "b", "c"], "val": "d"})
            )
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
            hbs.render("t", &serde_json::json!({"haystack": 42, "needle": "4"}))
                .unwrap(),
            "NO"
        );
        assert_eq!(
            hbs.render("t", &serde_json::json!({"haystack": true, "needle": "t"}))
                .unwrap(),
            "NO"
        );
        assert_eq!(
            hbs.render("t", &serde_json::json!({"haystack": null, "needle": "x"}))
                .unwrap(),
            "NO"
        );
        assert_eq!(
            hbs.render(
                "t",
                &serde_json::json!({"haystack": {"a": 1}, "needle": "a"})
            )
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
            hbs.render(
                "t",
                &serde_json::json!({"haystack": "hello 42 world", "needle": 42})
            )
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
            hbs.render("t", &serde_json::json!({"arr": [1, 2, 3], "val": 2}))
                .unwrap(),
            "YES"
        );
        assert_eq!(
            hbs.render("t", &serde_json::json!({"arr": [1, 2, 3], "val": 4}))
                .unwrap(),
            "NO"
        );
    }
}
