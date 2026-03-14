use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::Value;

/// Handlebars helper that renders the appropriate field partial.
/// Usage: {{render_field field_context}}
pub(super) struct RenderFieldHelper;

impl HelperDef for RenderFieldHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        r: &'reg Handlebars<'reg>,
        ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let param = h.param(0).ok_or_else(|| {
            RenderError::from(handlebars::RenderErrorReason::ParamNotFoundForIndex(
                "render_field",
                0,
            ))
        })?;

        let field_data = param.value();
        let field_type = field_data
            .get("field_type")
            .and_then(|v| v.as_str())
            .unwrap_or("text");

        let template_name = format!("fields/{}", field_type);

        // Inject _locale from parent context so {{t}} works inside field partials
        let render_data = if let Some(locale) = ctx.data().get("_locale") {
            let mut data = field_data.clone();
            if let Some(obj) = data.as_object_mut() {
                obj.insert("_locale".to_string(), locale.clone());
            }
            data
        } else {
            field_data.clone()
        };

        let rendered = r.render(&template_name, &render_data).unwrap_or_else(|e| {
            tracing::warn!(
                "Failed to render template '{}': {}, falling back to fields/text",
                template_name,
                e
            );
            r.render("fields/text", &render_data).unwrap_or_default()
        });

        Ok(ScopedJson::Derived(Value::String(rendered)))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn test_hbs() -> Handlebars<'static> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations =
            std::sync::Arc::new(crate::admin::translations::Translations::load(tmp.path()));
        let hbs = crate::admin::templates::create_handlebars(tmp.path(), false, translations)
            .expect("create_handlebars");
        (*hbs).clone()
    }

    #[test]
    fn renders_text_field() {
        let mut hbs = test_hbs();
        hbs.register_template_string("fields/text", "TEXT:{{name}}")
            .unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}")
            .unwrap();
        let result = hbs
            .render(
                "t",
                &json!({"ctx": {"field_type": "text", "name": "title"}}),
            )
            .unwrap();
        assert_eq!(result, "TEXT:title");
    }

    #[test]
    fn fallback_to_text_on_unknown_type() {
        let mut hbs = test_hbs();
        hbs.register_template_string("fields/text", "FALLBACK:{{name}}")
            .unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}")
            .unwrap();
        let result = hbs
            .render(
                "t",
                &json!({"ctx": {"field_type": "unknown_type", "name": "my_field"}}),
            )
            .unwrap();
        assert_eq!(result, "FALLBACK:my_field");
    }

    #[test]
    fn default_type_is_text() {
        let mut hbs = test_hbs();
        hbs.register_template_string("fields/text", "DEFAULT_TEXT:{{name}}")
            .unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}")
            .unwrap();
        let result = hbs
            .render("t", &json!({"ctx": {"name": "untitled"}}))
            .unwrap();
        assert_eq!(result, "DEFAULT_TEXT:untitled");
    }

    #[test]
    fn renders_select_field() {
        let mut hbs = test_hbs();
        hbs.register_template_string("fields/select", "SELECT:{{name}}")
            .unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}")
            .unwrap();
        let result = hbs
            .render(
                "t",
                &json!({"ctx": {"field_type": "select", "name": "status"}}),
            )
            .unwrap();
        assert_eq!(result, "SELECT:status");
    }

    #[test]
    fn renders_checkbox_field() {
        let mut hbs = test_hbs();
        hbs.register_template_string("fields/checkbox", "CHECKBOX:{{name}}")
            .unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}")
            .unwrap();
        let result = hbs
            .render(
                "t",
                &json!({"ctx": {"field_type": "checkbox", "name": "active"}}),
            )
            .unwrap();
        assert_eq!(result, "CHECKBOX:active");
    }
}
