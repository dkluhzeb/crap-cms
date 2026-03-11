use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};

use super::is_truthy;

/// Default value helper: `{{default val fallback}}`.
pub(super) struct DefaultHelper;

impl HelperDef for DefaultHelper {
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
        let fallback = h
            .param(1)
            .map(|p| p.value())
            .unwrap_or(&serde_json::Value::Null);
        if is_truthy(val) {
            Ok(ScopedJson::Derived(val.clone()))
        } else {
            Ok(ScopedJson::Derived(fallback.clone()))
        }
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
    fn default_basic() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{default val fallback}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &serde_json::json!({"val": "hello", "fallback": "bye"}))
                .unwrap(),
            "hello"
        );
        assert_eq!(
            hbs.render("t", &serde_json::json!({"val": null, "fallback": "bye"}))
                .unwrap(),
            "bye"
        );
        assert_eq!(
            hbs.render("t", &serde_json::json!({"val": "", "fallback": "bye"}))
                .unwrap(),
            "bye"
        );
    }

    #[test]
    fn default_false_uses_fallback() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{default val fallback}}")
            .unwrap();
        assert_eq!(
            hbs.render(
                "t",
                &serde_json::json!({"val": false, "fallback": "fallback_val"})
            )
            .unwrap(),
            "fallback_val"
        );
    }

    #[test]
    fn default_zero_uses_fallback() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{default val fallback}}")
            .unwrap();
        assert_eq!(
            hbs.render(
                "t",
                &serde_json::json!({"val": 0, "fallback": "fallback_val"})
            )
            .unwrap(),
            "fallback_val"
        );
    }

    #[test]
    fn default_empty_array_uses_fallback() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{default val fallback}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &serde_json::json!({"val": [], "fallback": "none"}))
                .unwrap(),
            "none"
        );
    }

    #[test]
    fn default_truthy_number() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{default val fallback}}")
            .unwrap();
        assert_eq!(
            hbs.render("t", &serde_json::json!({"val": 42, "fallback": "nope"}))
                .unwrap(),
            "42"
        );
    }
}
