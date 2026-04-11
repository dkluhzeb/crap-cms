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

        hbs
    })
}

/// Register a compiled-in template. Panics on parse errors — these templates are
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
