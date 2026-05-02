use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::Value;
use tracing::warn;

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

        // Per-instance template binding wins over the type-based default.
        // `admin.template` (Lua-side) is parsed into `FieldAdmin.template`
        // and serialized flat into the field render context as `template`
        // (matching the convention used by `label`, `placeholder`, etc.).
        // Path safety is enforced at field-parse time via
        // `validate_template_name`, so any value reaching here has
        // passed the whitelist.
        let template_name = field_data
            .get("template")
            .and_then(|t| t.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("fields/{}", field_type));

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
            warn!(
                "Failed to render template '{}': {}, falling back to fields/text",
                template_name, e
            );
            r.render("fields/text", &render_data).unwrap_or_default()
        });

        Ok(ScopedJson::Derived(Value::String(rendered)))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::admin::templates::helpers::test_helpers::test_hbs;

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
    fn admin_template_overrides_default_type_lookup() {
        // The rating example: a `number`-typed field opts into a custom
        // template via Lua-side `admin.template = "fields/rating"`,
        // which the field-context builder serializes flat as the
        // top-level `template` key. The helper reads it and skips
        // the default `fields/number` lookup.
        let mut hbs = test_hbs();
        hbs.register_template_string("fields/number", "DEFAULT_NUMBER:{{name}}")
            .unwrap();
        hbs.register_template_string("fields/rating", "STARS:{{name}}={{value}}")
            .unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}")
            .unwrap();
        let result = hbs
            .render(
                "t",
                &json!({"ctx": {
                    "field_type": "number",
                    "name": "rating",
                    "value": 4,
                    "template": "fields/rating",
                }}),
            )
            .unwrap();
        assert_eq!(result, "STARS:rating=4");
    }

    #[test]
    fn admin_template_falls_back_to_text_when_missing() {
        // If the per-field `template` key points at a template that
        // doesn't exist (extracted but later renamed, typo, etc.), the
        // helper logs a warning and falls back to fields/text — same as
        // the default `fields/<unknown_type>` failure path.
        let mut hbs = test_hbs();
        hbs.register_template_string("fields/text", "FALLBACK:{{name}}")
            .unwrap();
        hbs.register_template_string("t", "{{{render_field ctx}}}")
            .unwrap();
        let result = hbs
            .render(
                "t",
                &json!({"ctx": {
                    "field_type": "number",
                    "name": "rating",
                    "template": "fields/does-not-exist",
                }}),
            )
            .unwrap();
        assert_eq!(result, "FALLBACK:rating");
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
