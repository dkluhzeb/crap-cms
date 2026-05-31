//! Shared Handlebars renderer for scaffold templates.

use std::sync::OnceLock;

use anyhow::Result;
use handlebars::Handlebars;
use serde::Serialize;

/// Get the shared scaffold Handlebars registry, initializing on first use.
fn registry() -> &'static Handlebars<'static> {
    static HBS: OnceLock<Handlebars<'static>> = OnceLock::new();

    HBS.get_or_init(|| {
        let mut hbs = Handlebars::new();
        hbs.set_strict_mode(false);

        // Collection
        reg(
            &mut hbs,
            "collection",
            include_str!("collection/templates/collection.lua.hbs"),
        );

        // Global
        reg(
            &mut hbs,
            "global",
            include_str!("global/templates/global.lua.hbs"),
        );

        // Hook
        reg(
            &mut hbs,
            "hook_collection",
            include_str!("hook/templates/collection_hook.lua.hbs"),
        );
        reg(
            &mut hbs,
            "hook_field",
            include_str!("hook/templates/field_hook.lua.hbs"),
        );
        reg(
            &mut hbs,
            "hook_access",
            include_str!("hook/templates/access_hook.lua.hbs"),
        );
        reg(
            &mut hbs,
            "hook_condition_boolean",
            include_str!("hook/templates/condition_boolean.lua.hbs"),
        );
        reg(
            &mut hbs,
            "hook_condition_table",
            include_str!("hook/templates/condition_table.lua.hbs"),
        );

        // Job
        reg(&mut hbs, "job", include_str!("job/templates/job.lua.hbs"));

        // Init
        reg(
            &mut hbs,
            "crap_toml",
            include_str!("init/templates/crap.toml.hbs"),
        );

        // Migration
        reg(
            &mut hbs,
            "migration",
            include_str!("migration/templates/migration.lua.tpl"),
        );

        // Component / theme / node
        reg(
            &mut hbs,
            "component",
            include_str!("component/templates/component.js.hbs"),
        );
        reg(
            &mut hbs,
            "theme",
            include_str!("theme/templates/theme.css.hbs"),
        );
        reg(
            &mut hbs,
            "node",
            include_str!("node/templates/node.lua.hbs"),
        );

        // Field (template + Lua plugin)
        reg(
            &mut hbs,
            "field_template",
            include_str!("field/templates/field.hbs.hbs"),
        );
        reg(
            &mut hbs,
            "field_plugin",
            include_str!("field/templates/plugin.lua.hbs"),
        );

        // Page / slot -- produce .hbs files; templates use \{{ to escape
        // expressions that should appear literally in the output.
        reg(
            &mut hbs,
            "page",
            include_str!("page/templates/page.hbs.hbs"),
        );
        reg(
            &mut hbs,
            "slot",
            include_str!("slot/templates/slot.hbs.hbs"),
        );

        hbs
    })
}

/// Register a compiled-in template. Panics on parse errors -- these templates are
/// embedded via `include_str!` and a parse failure is a developer bug, not a
/// runtime condition (analogous to `Regex::new("literal").unwrap()`).
fn reg(hbs: &mut Handlebars, name: &str, content: &str) {
    hbs.register_template_string(name, content)
        .unwrap_or_else(|e| panic!("Failed to parse scaffold template '{name}': {e}"));
}

/// Render a scaffold template with the given context.
pub fn render(template: &str, ctx: &impl Serialize) -> Result<String> {
    registry().render(template, ctx).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every compiled-in scaffold template must parse (the registry panics
    /// otherwise) and render with a permissive (empty) context — catching a
    /// malformed `.hbs` at test time rather than when a user runs `make_*`.
    #[test]
    fn all_registered_templates_parse_and_render() {
        let names = [
            "collection",
            "global",
            "hook_collection",
            "hook_field",
            "hook_access",
            "hook_condition_boolean",
            "hook_condition_table",
            "job",
            "crap_toml",
            "migration",
            "component",
            "theme",
            "node",
            "field_template",
            "field_plugin",
            "page",
            "slot",
        ];
        for name in names {
            assert!(
                render(name, &json!({})).is_ok(),
                "template '{name}' failed to render"
            );
        }
    }

    #[test]
    fn unknown_template_name_errors() {
        assert!(render("does_not_exist", &json!({})).is_err());
    }
}
