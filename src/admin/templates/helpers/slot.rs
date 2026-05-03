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
//! ## Hash params (per-invocation data)
//!
//! Slots can pass named values to the slot file:
//!
//! ```hbs
//! {{slot "field_help" name=field.name kind=field.field_type}}
//! ```
//!
//! Inside `slots/field_help/<file>.hbs`, the values are available as
//! `{{name}}` and `{{kind}}` directly. The page context is still
//! reachable via `{{@root.user.email}}` etc., so slot files can mix
//! per-invocation data with page-level fallback.
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
//! page context (with hash params merged on top, when supplied).

use handlebars::{
    Context, Handlebars, Helper, HelperDef, Output, RenderContext, RenderError, Renderable,
};
use serde_json::{Map, Value};
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

        // Build the slot-file render context. Default = page ctx as-is.
        // With hash params (`{{slot "name" foo=bar}}`), the params are
        // merged on top so slot files see them at the root: `{{foo}}`.
        // Page-level data remains reachable via `@root` regardless.
        let render_ctx = merge_hash_into_context(ctx.data(), h);

        // Render each slot template, in sort order. Overlay files at the
        // same name as embedded ones win already (the registry handled
        // overlay precedence at load time).
        for name in names {
            match r.render(&name, &render_ctx) {
                Ok(s) => out.write(&s)?,
                Err(e) => warn!("Slot template '{}' render error: {}", name, e),
            }
        }

        Ok(())
    }
}

/// If the helper invocation has hash params (`foo=bar baz=qux`), merge
/// them on top of the page context so slot files see them at the root.
/// When the page ctx isn't an object (rare — defensive fallback), fall
/// back to a fresh object built from the hash. With no hash params,
/// returns the page ctx unchanged via `clone`.
fn merge_hash_into_context(page_ctx: &Value, h: &Helper<'_>) -> Value {
    let hash = h.hash();
    if hash.is_empty() {
        return page_ctx.clone();
    }

    let mut map = match page_ctx {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };

    for (key, path_and_json) in hash {
        map.insert((*key).to_string(), path_and_json.value().clone());
    }

    Value::Object(map)
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

    #[test]
    fn hash_params_are_visible_at_slot_root() {
        let mut hbs = test_hbs();
        hbs.register_template_string("slots/field_help/x", "name={{name}};kind={{kind}}")
            .unwrap();
        hbs.register_template_string("page", r#"{{slot "field_help" name="title" kind="text"}}"#)
            .unwrap();

        let result = hbs.render("page", &json!({})).unwrap();
        assert_eq!(result, "name=title;kind=text");
    }

    #[test]
    fn hash_params_overlay_page_context_and_root_remains_reachable() {
        let mut hbs = test_hbs();
        hbs.register_template_string(
            "slots/section/widget",
            // {{name}} is the hash override; {{@root.user.email}} reaches past the merge.
            "field={{name}};editor={{@root.user.email}}",
        )
        .unwrap();
        hbs.register_template_string("page", r#"{{slot "section" name="title"}}"#)
            .unwrap();

        let result = hbs
            .render("page", &json!({ "user": { "email": "alice@example.com" } }))
            .unwrap();
        assert_eq!(result, "field=title;editor=alice@example.com");
    }

    #[test]
    fn hash_params_can_pass_objects_for_per_instance_slots() {
        let mut hbs = test_hbs();
        hbs.register_template_string(
            "slots/field_help/x",
            "{{field.label}} ({{field.field_type}})",
        )
        .unwrap();
        hbs.register_template_string(
            "page",
            // Loop pattern: each field invokes the slot with itself as `field`.
            r#"{{#each fields}}{{slot "field_help" field=this}}{{/each}}"#,
        )
        .unwrap();

        let result = hbs
            .render(
                "page",
                &json!({
                    "fields": [
                        { "label": "Title", "field_type": "text" },
                        { "label": "Body", "field_type": "richtext" },
                    ]
                }),
            )
            .unwrap();
        assert_eq!(result, "Title (text)Body (richtext)");
    }
}
