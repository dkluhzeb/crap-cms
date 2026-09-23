//! The per-global classes: hook data, the write-shape partial payload, the
//! read-shape document, the hook contexts and the function aliases.

use crate::{
    core::{
        collection::GlobalDefinition,
        upload::{readable_fields, writable_fields},
    },
    typegen::helpers::{to_pascal_case, w},
};

use super::{
    accessor::{render_global_accessor, render_global_typing_factories},
    classes::{
        render_sub_type_classes, write_fields, write_hook_context_tail, write_system_fields,
    },
    field::LuaShape,
};

/// The operations an `after_read` hook of a global runs for: the read, and
/// the update that produced a live event.
const GLOBAL_READ_HOOK_OPERATIONS: &str = r#""get" | "update""#;

/// The hook-data, partial-payload and document classes of a global.
fn render_data_classes(out: &mut String, global: &GlobalDefinition, pascal: &str) {
    let read_fields = readable_fields(&global.fields);

    // crap.global_data.* — hook ctx.data for globals. `id` and timestamps
    // are emitted optional for the same reason as `crap.data.X`.
    w!(out, "---@class crap.global_data.{pascal}");
    w!(out, "---@field id? string");
    write_fields(out, &global.fields, pascal, LuaShape::Input);
    w!(out, "---@field created_at? string");
    w!(out, "---@field updated_at? string");
    out.push('\n');

    // crap.global_partial.* — partial-update payload for `update`: the write
    // shape, every field optional.
    w!(out, "---@class crap.global_partial.{pascal}");
    write_fields(
        out,
        &writable_fields(&global.fields),
        pascal,
        LuaShape::Partial,
    );
    out.push('\n');

    // crap.global_doc.* — the read shape; always has timestamps. Same
    // subclass reasoning as `crap.doc.{pascal}`.
    w!(out, "---@class crap.global_doc.{pascal} : crap.Document");
    w!(out, "---@field id string");
    write_fields(out, &read_fields, pascal, LuaShape::Read);
    write_system_fields(out, global.has_drafts(), false);
    w!(out, "---@field created_at? string");
    w!(out, "---@field updated_at? string");
    out.push('\n');
}

/// The typed hook contexts: `crap.hook.global_*` (`ctx.data` the stored
/// shape), `crap.read_hook.global_*` (`after_read`, `ctx.data` the document
/// as the read returns it) and `crap.field_hook.global_*`.
fn render_hook_classes(out: &mut String, global: &GlobalDefinition, pascal: &str) {
    let slug = &global.slug;

    w!(out, "---@class crap.hook.global_{slug}");
    w!(out, "---@field collection \"{slug}\"");
    w!(out, "---@field operation \"update\" | \"get\"");
    w!(out, "---@field data crap.global_data.{pascal}");
    write_hook_context_tail(out);

    w!(out, "---@class crap.read_hook.global_{slug}");
    w!(out, "---@field collection \"{slug}\"");
    w!(out, "---@field operation {GLOBAL_READ_HOOK_OPERATIONS}");
    w!(out, "---@field data crap.global_doc.{pascal}");
    write_hook_context_tail(out);

    w!(out, "---@class crap.field_hook.global_{slug}");
    w!(out, "---@field field_name string");
    w!(out, "---@field collection \"{slug}\"");
    w!(out, "---@field operation string");
    w!(out, "---@field data crap.global_data.{pascal}");
    w!(out, "---@field user? table");
    w!(out, "---@field ui_locale? string");
    out.push('\n');
}

/// Function-type aliases for global hooks — same pattern as collections: a
/// `---@type` cast replaces 3+ annotation lines.
fn render_fn_aliases(out: &mut String, slug: &str, pascal: &str) {
    w!(
        out,
        "---@alias crap.hook_fn.global_{slug} fun(ctx: crap.hook.global_{slug}): crap.hook.global_{slug}"
    );
    w!(
        out,
        "---@alias crap.read_hook_fn.global_{slug} fun(ctx: crap.read_hook.global_{slug}): crap.read_hook.global_{slug}"
    );
    w!(
        out,
        "---@alias crap.field_hook_fn.global_{slug} fun(value: any, context: crap.field_hook.global_{slug}): any"
    );
    w!(
        out,
        "---@alias crap.display_condition_fn.global_{slug} fun(data: crap.global_data.{pascal}, ctx: crap.ConditionContext): boolean | table"
    );
    out.push('\n');
}

/// Render type definitions for a single global.
pub(super) fn render_global(out: &mut String, global: &GlobalDefinition) {
    let pascal = to_pascal_case(&global.slug);

    // The stored write-shape family (a virtual `Join` is never stored) and
    // the read-shape family, as for a collection.
    render_sub_type_classes(
        out,
        &writable_fields(&global.fields),
        &pascal,
        LuaShape::Input,
    );
    render_sub_type_classes(
        out,
        &readable_fields(&global.fields),
        &pascal,
        LuaShape::Read,
    );

    render_data_classes(out, global, &pascal);
    render_hook_classes(out, global, &pascal);
    render_fn_aliases(out, &global.slug, &pascal);

    render_global_accessor(out, &global.slug, &pascal);
    render_global_typing_factories(out, global, &pascal);
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{class_block, text_field};
    use super::*;
    use crate::core::{FieldDefinition, FieldType};

    /// Global reads are `get`.
    #[test]
    fn hook_context_operations_match_the_runtime() {
        let global = GlobalDefinition::new("footer");
        let mut out = String::new();
        render_global(&mut out, &global);
        assert!(
            out.contains(r#"---@field operation "update" | "get""#),
            "{out}"
        );
    }

    /// Regression: a global's `after_read` hooks were typed with the stored
    /// hook data, although their `ctx.data` is the read document.
    #[test]
    fn after_read_hooks_get_the_read_shape() {
        let mut global = GlobalDefinition::new("footer");
        global.fields = vec![text_field("copyright", true)];
        let mut out = String::new();
        render_global(&mut out, &global);

        let read_hook = class_block(&out, "---@class crap.read_hook.global_footer");
        assert!(
            read_hook.contains("---@field data crap.global_doc.Footer"),
            "{read_hook}"
        );
        assert!(
            out.contains("---@alias crap.read_hook_fn.global_footer"),
            "{out}"
        );
    }

    #[test]
    fn render_global_output() {
        let mut global = GlobalDefinition::new("site_settings");
        global.fields = vec![text_field("site_name", true), text_field("tagline", false)];

        let mut out = String::new();
        render_global(&mut out, &global);

        assert!(out.contains("---@class crap.global_data.SiteSettings"));
        assert!(out.contains("---@field site_name string"));
        assert!(out.contains("---@field tagline? string"));
        assert!(out.contains("---@class crap.global_doc.SiteSettings : crap.Document"));
        assert!(out.contains("---@class crap.hook.global_site_settings"));
        assert!(out.contains("---@class crap.field_hook.global_site_settings"));

        // Accessor binds all four CRUD-ish methods, matching the runtime
        // GLOBAL_METHODS list (get/update/unpublish/validate).
        assert!(out.contains("function _glob_site_settings.get(opts) end"));
        assert!(out.contains("function _glob_site_settings.update(data, opts) end"));
        assert!(out.contains("function _glob_site_settings.unpublish(opts) end"));
        assert!(out.contains("function _glob_site_settings.validate(data, opts) end"));
    }

    #[test]
    fn global_hook_context_uses_collection_field() {
        let mut global = GlobalDefinition::new("site_settings");
        global.fields = vec![text_field("site_name", true)];

        let mut out = String::new();
        render_global(&mut out, &global);

        // Runtime sets "collection" not "global" — verify generated types match
        let hook = class_block(&out, "---@class crap.hook.global_site_settings");
        assert!(
            hook.contains("---@field collection \"site_settings\""),
            "global hook context must use 'collection' field (matching runtime), got:\n{hook}"
        );
        assert!(
            !out.contains("---@field global"),
            "global hook context must NOT use 'global' field"
        );

        // user and ui_locale must be present
        assert!(hook.contains("---@field user? table"), "{hook}");
        assert!(hook.contains("---@field ui_locale? string"), "{hook}");
    }

    #[test]
    fn render_global_array_row() {
        let mut global = GlobalDefinition::new("navigation");
        global.fields = vec![
            FieldDefinition::builder("main_nav", FieldType::Array)
                .fields(vec![text_field("label", true), text_field("url", true)])
                .build(),
        ];

        let mut out = String::new();
        render_global(&mut out, &global);

        assert!(
            out.contains("---@class crap.array_row.NavigationMainNav"),
            "global array should emit prefixed sub-type class, got:\n{out}"
        );
        assert!(out.contains("---@field label string"));
    }

    /// Regression: a nested array inside an array row must reference
    /// the inner sub-type by its compound parent path (e.g.
    /// `NavigationMainNavChildren`) AND emit a matching declaration at
    /// the same name. Previously the declaration used only the outer
    /// global's `PascalCase` (e.g. `NavigationChildren`), leaving the
    /// reference dangling with an "`Undefined` type" `LuaLS` warning.
    #[test]
    fn render_global_nested_array_row_name_matches_reference() {
        let mut global = GlobalDefinition::new("navigation");
        global.fields = vec![
            FieldDefinition::builder("main_nav", FieldType::Array)
                .fields(vec![
                    text_field("label", true),
                    FieldDefinition::builder("children", FieldType::Array)
                        .fields(vec![text_field("url", true)])
                        .build(),
                ])
                .build(),
        ];

        let mut out = String::new();
        render_global(&mut out, &global);

        assert!(
            out.contains("---@class crap.array_row.NavigationMainNav"),
            "outer array sub-type class missing, got:\n{out}"
        );
        assert!(
            out.contains("---@class crap.array_row.NavigationMainNavChildren"),
            "nested array sub-type class must carry the compound parent path, got:\n{out}"
        );
        assert!(
            out.contains("---@field children? crap.array_row.NavigationMainNavChildren[]"),
            "outer row must reference the inner sub-type by its compound name, got:\n{out}"
        );
        assert!(
            !out.contains("---@class crap.array_row.NavigationChildren\n"),
            "must not emit the bare (un-compounded) inner sub-type name, got:\n{out}"
        );
    }

    #[test]
    fn render_global_hook_context_fields() {
        let mut global = GlobalDefinition::new("footer");
        global.fields = vec![text_field("copyright", true)];

        let mut out = String::new();
        render_global(&mut out, &global);

        assert!(
            out.contains("---@field hook_depth integer"),
            "global hook context should have hook_depth"
        );
        assert!(
            out.contains("---@field draft? boolean"),
            "global hook context should have draft"
        );
        assert!(
            out.contains("---@field context table<string, any>"),
            "global hook context should have context"
        );
    }

    #[test]
    fn lua_group_subtype_emitted_in_global() {
        let mut global = GlobalDefinition::new("settings");
        global.fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text_field("title", true)])
                .build(),
        ];
        let mut out = String::new();
        render_global(&mut out, &global);
        assert!(
            out.contains("---@class crap.group.SettingsSeo"),
            "global group sub-type should be prefixed: {out}"
        );
    }

    /// A global's partial payload follows the write shape and its document
    /// the read shape; a global is never populated, so it has no tag.
    #[test]
    fn global_classes_follow_the_wire_shapes() {
        let mut global = GlobalDefinition::new("settings");
        global.fields = vec![
            text_field("site_name", true),
            FieldDefinition::builder("secret", FieldType::Text)
                .hidden(true)
                .build(),
            FieldDefinition::builder("refs", FieldType::Join).build(),
        ];

        let mut out = String::new();
        render_global(&mut out, &global);

        let partial = class_block(&out, "---@class crap.global_partial.Settings");
        assert!(partial.contains("---@field secret? string"), "{partial}");
        assert!(!partial.contains("refs"), "{partial}");

        let doc = class_block(&out, "---@class crap.global_doc.Settings");
        assert!(doc.contains("---@field refs? table[]"), "{doc}");
        assert!(!doc.contains("secret"), "{doc}");
        assert!(!doc.contains("collection?"), "{doc}");
    }
}
