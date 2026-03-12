use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::Value;

/// String concatenation helper: `{{concat a b c}}`.
pub(super) struct ConcatHelper;

impl HelperDef for ConcatHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let mut result = String::new();
        for i in 0.. {
            match h.param(i) {
                Some(p) => match p.value() {
                    Value::String(s) => result.push_str(s),
                    Value::Number(n) => result.push_str(&n.to_string()),
                    Value::Bool(b) => result.push_str(&b.to_string()),
                    Value::Null => {}
                    other => result.push_str(&other.to_string()),
                },
                None => break,
            }
        }
        Ok(ScopedJson::Derived(Value::String(result)))
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
    fn concat_strings() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a b c}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": "hello", "b": " ", "c": "world"}))
                .unwrap(),
            "hello world"
        );
    }

    #[test]
    fn concat_numbers() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a b}}").unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": "count:", "b": 42})).unwrap(),
            "count:42"
        );
    }

    #[test]
    fn concat_bools() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a b}}").unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": "is:", "b": true})).unwrap(),
            "is:true"
        );
    }

    #[test]
    fn concat_null() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a b c}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &json!({"a": "hello", "b": null, "c": "world"}))
                .unwrap(),
            "helloworld"
        );
    }

    #[test]
    fn concat_array_value() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a b}}").unwrap();
        let result = hbs
            .render("t", &json!({"a": "data:", "b": [1, 2]}))
            .unwrap();
        assert!(result.starts_with("data:"));
        assert!(result.contains("[1,2]"));
    }

    #[test]
    fn concat_no_params() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat}}").unwrap();
        assert_eq!(hbs.render("t", &json!({})).unwrap(), "");
    }

    #[test]
    fn concat_single_param() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{concat a}}").unwrap();
        assert_eq!(hbs.render("t", &json!({"a": "only"})).unwrap(), "only");
    }
}
