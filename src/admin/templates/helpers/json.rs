use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};

/// JSON serialization helper: `{{{json value}}}`.
pub(super) struct JsonHelper;

impl HelperDef for JsonHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let val = h
            .param(0)
            .map(|p| p.value())
            .unwrap_or(&serde_json::Value::Null);
        let json_str = serde_json::to_string(val).unwrap_or_default();
        Ok(ScopedJson::Derived(serde_json::Value::String(json_str)))
    }
}

#[cfg(test)]
mod tests {
    fn test_hbs() -> handlebars::Handlebars<'static> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations = std::sync::Arc::new(crate::admin::translations::Translations::load(tmp.path()));
        let hbs = crate::admin::templates::create_handlebars(tmp.path(), false, translations)
            .expect("create_handlebars");
        (*hbs).clone()
    }

    #[test]
    fn json_object() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs
            .render("t", &serde_json::json!({"val": {"key": "value"}}))
            .unwrap();
        assert!(result.contains("\"key\""));
        assert!(result.contains("\"value\""));
    }

    #[test]
    fn json_null() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({"val": null})).unwrap();
        assert_eq!(result, "null");
    }

    #[test]
    fn json_array() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs
            .render("t", &serde_json::json!({"val": [1, "two", true]}))
            .unwrap();
        assert_eq!(result, r#"[1,"two",true]"#);
    }

    #[test]
    fn json_string() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs
            .render("t", &serde_json::json!({"val": "hello"}))
            .unwrap();
        assert_eq!(result, "\"hello\"");
    }

    #[test]
    fn json_no_param() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json}}}").unwrap();
        let result = hbs.render("t", &serde_json::json!({})).unwrap();
        assert_eq!(result, "null");
    }
}
