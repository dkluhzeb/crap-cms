//! The building blocks every per-collection and per-global class set shares:
//! the stored system keys, the array-row / group sub-type classes, a field
//! list in one wire shape, and the typed hook-context tail. The collection
//! classes live in [`super::collection_classes`], the global ones in
//! [`super::global_classes`].

use crate::{
    core::FieldDefinition,
    typegen::helpers::{
        DRAFT_STATUS_VALUES, SubTypeKind, collect_sub_type_fields, to_pascal_case, w,
    },
};

use super::field::{LuaShape, write_field};

/// The stored system keys a read document carries besides its fields.
pub(super) fn write_system_fields(out: &mut String, drafts: bool, soft_delete: bool) {
    if drafts {
        let values = DRAFT_STATUS_VALUES.map(|v| format!("\"{v}\""));
        w!(out, "---@field _status? {}", values.join(" | "));
    }

    if soft_delete {
        w!(out, "---@field _deleted_at? string");
    }
}

/// Emit the classes of an owner's array rows and group shapes in `shape`'s
/// namespaces. A relational array row carries its junction `id`.
pub(super) fn render_sub_type_classes(
    out: &mut String,
    fields: &[FieldDefinition],
    pascal: &str,
    shape: LuaShape,
) {
    let (row, group) = shape.sub_type_namespaces();

    for stf in collect_sub_type_fields(fields, pascal) {
        let sub_pascal = format!("{}{}", stf.parent_pascal, to_pascal_case(&stf.field.name));
        let namespace = match stf.kind {
            SubTypeKind::Array => row,
            SubTypeKind::Group => group,
        };
        w!(out, "---@class crap.{namespace}.{sub_pascal}");
        if stf.row_id {
            w!(out, "---@field id? string");
        }
        for sf in &stf.field.fields {
            write_field(out, sf, &sub_pascal, shape);
        }
        out.push('\n');
    }
}

/// Write every field of `fields` in `shape`.
pub(super) fn write_fields(
    out: &mut String,
    fields: &[FieldDefinition],
    pascal: &str,
    shape: LuaShape,
) {
    for f in fields {
        write_field(out, f, pascal, shape);
    }
}

/// The keys every typed hook context carries after its `collection`,
/// `operation` and `data` lines — the table the runtime builds for a hook
/// (`crap.HookContext`), closing the class.
pub(super) fn write_hook_context_tail(out: &mut String) {
    w!(out, "---@field id? string");
    w!(out, "---@field context table<string, any>");
    w!(out, "---@field hook_depth integer");
    w!(out, "---@field locale? string");
    w!(out, "---@field draft? boolean");
    w!(out, "---@field user? table");
    w!(out, "---@field ui_locale? string");
    w!(out, "---@field options? table");
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The typed hook contexts carry every key the runtime sets on a hook's
    /// context table, `ctx.id` and the per-ref `options` included.
    #[test]
    fn hook_context_tail_matches_the_runtime_table() {
        let mut out = String::new();
        write_hook_context_tail(&mut out);

        for line in [
            "---@field id? string",
            "---@field context table<string, any>",
            "---@field hook_depth integer",
            "---@field locale? string",
            "---@field draft? boolean",
            "---@field user? table",
            "---@field ui_locale? string",
            "---@field options? table",
        ] {
            assert!(out.contains(line), "{line}: {out}");
        }
    }

    /// A drafts collection's read carries `_status`; a soft-delete one
    /// `_deleted_at`; neither without the feature.
    #[test]
    fn system_fields_follow_the_features() {
        let mut out = String::new();
        write_system_fields(&mut out, false, false);
        assert!(out.is_empty(), "{out}");

        write_system_fields(&mut out, true, true);
        assert!(
            out.contains(r#"---@field _status? "draft" | "published""#),
            "{out}"
        );
        assert!(out.contains("---@field _deleted_at? string"), "{out}");
    }
}
