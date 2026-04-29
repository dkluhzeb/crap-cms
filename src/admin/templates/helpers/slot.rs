//! `{{slot "name"}}` helper — additive-extension point for templates.
//!
//! Built-in templates declare named slots:
//!
//! ```hbs
//! {{slot "dashboard_widgets"}}
//! ```
//!
//! Customizers drop one or more `.hbs` files at
//! `<config_dir>/templates/slots/dashboard_widgets/<anything>.hbs`. The
//! helper enumerates everything registered under the `slots/<name>/`
//! prefix, renders each in alphabetical order against the current page
//! context, and concatenates the output.
//!
//! ## Block form (fallback)
//!
//! ```hbs
//! {{#slot "dashboard_widgets"}}
//!   <p class="muted">Nothing pinned yet.</p>
//! {{/slot}}
//! ```
//!
//! When no slot files are present, the inline block body is rendered
//! instead. Both built-in fallbacks and overlay slot files use the same
//! page context.

use handlebars::{
    Context, Handlebars, Helper, HelperDef, Output, RenderContext, RenderError, Renderable,
};
use tracing::warn;

/// Handlebars helper that enumerates and renders slot templates. Writes
/// raw HTML directly to the output buffer (no escaping) — slot
/// contributions are full HTML fragments, not values to interpolate.
pub(super) struct SlotHelper;

impl HelperDef for SlotHelper {
    fn call<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        r: &'reg Handlebars<'reg>,
        ctx: &'rc Context,
        rc: &mut RenderContext<'reg, 'rc>,
        out: &mut dyn Output,
    ) -> Result<(), RenderError> {
        let slot_name = h
            .param(0)
            .and_then(|p| p.value().as_str().map(str::to_string))
            .ok_or_else(|| {
                RenderError::from(handlebars::RenderErrorReason::ParamNotFoundForIndex(
                    "slot", 0,
                ))
            })?;

        let prefix = format!("slots/{}/", slot_name);

        // Enumerate all registered templates under `slots/<name>/`.
        let mut names: Vec<String> = r
            .get_templates()
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        names.sort();

        // No slot contributions — fall back to the inline block, if any.
        if names.is_empty() {
            if let Some(template) = h.template() {
                template.render(r, ctx, rc, out)?;
            }
            return Ok(());
        }

        // Render each slot template against the current page context, in
        // sort order. Overlay files at the same name as embedded ones win
        // already (the registry handled overlay precedence at load time).
        for name in names {
            match r.render(&name, ctx.data()) {
                Ok(s) => out.write(&s)?,
                Err(e) => warn!("Slot template '{}' render error: {}", name, e),
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::admin::templates::helpers::test_helpers::test_hbs;

    #[test]
    fn renders_each_slot_file_in_sort_order() {
        let mut hbs = test_hbs();
        hbs.register_template_string("slots/dashboard_widgets/b_widget", "[B:{{user.email}}]")
            .unwrap();
        hbs.register_template_string("slots/dashboard_widgets/a_widget", "[A]")
            .unwrap();
        hbs.register_template_string("page", r#"{{slot "dashboard_widgets"}}"#)
            .unwrap();

        let result = hbs
            .render("page", &json!({ "user": { "email": "x@y" } }))
            .unwrap();
        assert_eq!(result, "[A][B:x@y]");
    }

    #[test]
    fn block_form_renders_fallback_when_no_slot_files() {
        let mut hbs = test_hbs();
        hbs.register_template_string("page", r#"{{#slot "missing"}}<p>fallback</p>{{/slot}}"#)
            .unwrap();

        let result = hbs.render("page", &json!({})).unwrap();
        assert_eq!(result, "<p>fallback</p>");
    }

    #[test]
    fn block_form_skips_fallback_when_slot_files_exist() {
        let mut hbs = test_hbs();
        hbs.register_template_string("slots/foo/x", "REAL").unwrap();
        hbs.register_template_string("page", r#"{{#slot "foo"}}<p>fallback</p>{{/slot}}"#)
            .unwrap();

        let result = hbs.render("page", &json!({})).unwrap();
        assert_eq!(result, "REAL");
    }

    #[test]
    fn inline_form_with_no_files_emits_nothing() {
        let mut hbs = test_hbs();
        hbs.register_template_string("page", r#"({{slot "absent"}})"#)
            .unwrap();
        let result = hbs.render("page", &json!({})).unwrap();
        assert_eq!(result, "()");
    }

    #[test]
    fn slot_files_share_the_page_context() {
        let mut hbs = test_hbs();
        hbs.register_template_string("slots/section/widget", "user={{user.email}}")
            .unwrap();
        hbs.register_template_string("page", r#"{{slot "section"}}"#)
            .unwrap();

        let result = hbs
            .render("page", &json!({ "user": { "email": "alice@example.com" } }))
            .unwrap();
        assert_eq!(result, "user=alice@example.com");
    }
}
