//! The building blocks every per-collection and per-global class set shares:
//! the stored system keys, the array-row / group sub-type classes, a field
//! list in one wire shape, and the typed hook-context tail. The collection
//! classes live in [`super::collection_classes`], the global ones in
//! [`super::global_classes`].

use crate::{
    core::{FieldChildren, FieldDefinition, field_children},
    typegen::helpers::{
        DRAFT_STATUS_VALUES, SubTypeKind, collect_sub_type_fields, to_pascal_case, w,
    },
};

use super::field::{
    LOCALIZED_GROUP_NAMESPACE, LuaShape, PARTIAL_GROUP_NAMESPACE, holds_localized_columns,
    write_field,
};

/// A Lua union of string literals: `"a" | "b"`.
pub(super) fn literal_union(values: &[&str]) -> String {
    values
        .iter()
        .map(|v| format!("\"{v}\""))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The stored system keys a read document carries besides its fields.
pub(super) fn write_system_fields(out: &mut String, drafts: bool, soft_delete: bool) {
    if drafts {
        w!(
            out,
            "---@field _status? {}",
            literal_union(&DRAFT_STATUS_VALUES)
        );
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

/// Emit the partial-write classes (`crap.group_partial.*`) of every group in
/// `fields` held on the owner's own row, nested groups included — named like
/// the group's write class, so a reference from a partial class resolves. A
/// partial update writes the sub-fields it sends and keeps the rest, so each
/// is optional. A group inside an array or blocks row is written whole with
/// its row and keeps its `crap.group.*` class.
pub(super) fn render_partial_group_classes(
    out: &mut String,
    fields: &[FieldDefinition],
    pascal: &str,
) {
    for field in fields {
        match field_children(field) {
            FieldChildren::Wrapper(sub) => render_partial_group_classes(out, sub, pascal),
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    render_partial_group_classes(out, &tab.fields, pascal);
                }
            }
            FieldChildren::Group(sub) if !sub.is_empty() => {
                let sub_pascal = format!("{pascal}{}", to_pascal_case(&field.name));

                w!(out, "---@class crap.{PARTIAL_GROUP_NAMESPACE}.{sub_pascal}");
                write_fields(out, sub, &sub_pascal, LuaShape::Partial);
                out.push('\n');

                render_partial_group_classes(out, sub, &sub_pascal);
            }
            _ => {}
        }
    }
}

/// Emit the `locale = "all"` read classes (`crap.doc_group_localized.*`) of
/// every group in `fields` holding a per-locale column, nested groups
/// included — named like the group's read class, so a reference from its
/// parent resolves.
pub(super) fn render_localized_group_classes(
    out: &mut String,
    fields: &[FieldDefinition],
    pascal: &str,
    shape: LuaShape,
) {
    for field in fields {
        match field_children(field) {
            FieldChildren::Wrapper(sub) => render_localized_group_classes(out, sub, pascal, shape),
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    render_localized_group_classes(out, &tab.fields, pascal, shape);
                }
            }
            FieldChildren::Group(sub) if holds_localized_columns(field, shape) => {
                let sub_pascal = format!("{pascal}{}", to_pascal_case(&field.name));
                let inner = shape.inside(field);

                w!(
                    out,
                    "---@class crap.{LOCALIZED_GROUP_NAMESPACE}.{sub_pascal}"
                );
                write_fields(out, sub, &sub_pascal, inner);
                out.push('\n');

                render_localized_group_classes(out, sub, &sub_pascal, inner);
            }
            _ => {}
        }
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
    w!(out, "---@field edited_by? {{ id: string, email: string }}");
    out.push('\n');
}

/// The keys of a typed field-hook context after its `collection` line — the
/// table the runtime builds for a field hook (`crap.FieldHookContext`),
/// closing the class. `document` is the owner's full document (`doc_class`);
/// `data` is the nearest scope, which is that document only for a top-level
/// field — a group object or an array/blocks row for a nested one.
pub(super) fn write_field_hook_context(out: &mut String, doc_class: &str) {
    w!(out, "---@field field_name string");
    w!(out, "---@field operation string");
    w!(out, "---@field id? string");
    w!(out, "---@field locale? string");
    w!(
        out,
        "---@field data {doc_class}|table<string, any> The nearest scope: the document for a top-level field, the group object or array/blocks row for a nested one"
    );
    w!(out, "---@field document {doc_class}");
    w!(out, "---@field user? table");
    w!(out, "---@field ui_locale? string");
    w!(out, "---@field options? table");
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        hooks::lifecycle::{FieldHookContext, HookContext},
        typegen::LuaAnnotation,
    };

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
            "---@field edited_by? { id: string, email: string }",
        ] {
            assert!(out.contains(line), "{line}: {out}");
        }
    }

    /// The typed field-hook contexts carry every key the runtime
    /// `crap.FieldHookContext` sets — `id`, `locale`, `document` and `options`
    /// included — and type `data` as the nearest scope, not the document.
    #[test]
    fn field_hook_context_matches_the_runtime_table() {
        let mut out = String::new();
        write_field_hook_context(&mut out, "crap.data.Posts");

        for line in [
            "---@field field_name string",
            "---@field operation string",
            "---@field id? string",
            "---@field locale? string",
            "---@field data crap.data.Posts|table<string, any> ",
            "---@field document crap.data.Posts",
            "---@field user? table",
            "---@field ui_locale? string",
            "---@field options? table",
        ] {
            assert!(out.contains(line), "{line}: {out}");
        }
    }

    /// The field names of a derive-rendered class, `?` stripped.
    fn runtime_field_names(render: fn(&mut String)) -> Vec<String> {
        let mut class = String::new();
        render(&mut class);

        class
            .lines()
            .filter_map(|line| line.strip_prefix("--- @field "))
            .filter_map(|rest| rest.split_whitespace().next())
            .map(|name| name.trim_end_matches('?').to_string())
            .collect()
    }

    /// Every key of the runtime contexts is declared on the per-collection
    /// typed contexts, so a new runtime key cannot be missed by the typed
    /// classes again.
    #[test]
    fn typed_contexts_declare_every_runtime_key() {
        let mut hook =
            String::from("---@field collection x\n---@field operation x\n---@field data x\n");
        write_hook_context_tail(&mut hook);

        for name in runtime_field_names(HookContext::render_lua_annotation) {
            assert!(
                hook.contains(&format!("---@field {name}")),
                "hook context lacks {name}"
            );
        }

        let mut field_hook = String::from("---@field collection x\n");
        write_field_hook_context(&mut field_hook, "crap.data.Posts");

        for name in runtime_field_names(FieldHookContext::render_lua_annotation) {
            assert!(
                field_hook.contains(&format!("---@field {name}")),
                "field hook context lacks {name}"
            );
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
