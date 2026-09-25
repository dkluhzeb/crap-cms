//! The per-collection classes: hook data, the write-shape input classes, the
//! read-shape document class, the hook contexts and the function aliases.

use crate::{
    core::{
        CollectionDefinition,
        upload::{read_shape_fields, writable_fields, write_shape_fields},
    },
    hooks::lifecycle::operation::{collection_hook_operations, collection_read_hook_operations},
    typegen::helpers::{COLLECTION_TAG_KEY, has_localized_columns, to_pascal_case, w},
};

use super::{
    accessor::{render_collection_accessor, render_collection_typing_factories},
    classes::{
        literal_union, render_localized_group_classes, render_partial_group_classes,
        render_sub_type_classes, write_field_hook_context, write_fields, write_hook_context_tail,
        write_system_fields,
    },
    field::LuaShape,
    query_classes::render_query_classes,
};

/// `crap.input.*`, `crap.partial_many.*` and `crap.partial.*` — what `create`,
/// `update_many` and `update` accept: the write shape (no virtual `Join`, no
/// server-derived upload column), each reference as its id, plus an auth
/// collection's `password` where the operation takes one.
fn render_input_classes(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    let fields = write_shape_fields(col);
    let password = col.is_auth_collection();

    // crap.input.* — the `create` / `create_many` / `validate` payload; a
    // required field stays required.
    w!(out, "---@class crap.input.{pascal}");
    write_fields(out, &fields, pascal, LuaShape::Input);
    if password {
        w!(
            out,
            "---@field password? string Hashed on write, never read back"
        );
    }
    out.push('\n');

    // crap.partial_many.* — the `update_many` payload: only the fields the
    // caller wants to change, so every one is optional. Never a password —
    // `update_many` refuses one.
    w!(out, "---@class crap.partial_many.{pascal}");
    write_fields(out, &fields, pascal, LuaShape::Partial);
    out.push('\n');

    // crap.partial.* — the single `update` payload: the same fields, plus
    // an auth collection's password.
    w!(
        out,
        "---@class crap.partial.{pascal} : crap.partial_many.{pascal}"
    );
    if password {
        w!(
            out,
            "---@field password? string Replaces the stored password (empty keeps it)"
        );
    }
    out.push('\n');
}

/// `crap.doc.*` — the returned document. Inherits from `crap.Document` so
/// functions annotated `@return crap.Document` (e.g.
/// `crap.collections.find_by_id`, custom auth strategies) accept the
/// per-collection types without union-mismatch diagnostics from
/// lua-language-server. The read shape: hidden fields stripped, an upload's
/// per-size columns folded into `sizes`, every field optional (a draft may
/// lack required values, and field read access and `select` drop keys), a
/// reference populated at the default depth.
fn render_doc_class(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    let read_fields = read_shape_fields(col);

    w!(out, "---@class crap.doc.{pascal} : crap.Document");
    w!(out, "---@field id string");
    write_fields(out, &read_fields, pascal, LuaShape::Read);
    write_doc_keys(out, col);
}

/// The keys a returned document carries besides its fields — the stored
/// system keys, the populated `collection` tag and the timestamps — closing
/// the class.
fn write_doc_keys(out: &mut String, col: &CollectionDefinition) {
    write_system_fields(out, col.has_drafts(), col.soft_delete);

    w!(
        out,
        "---@field {COLLECTION_TAG_KEY}? \"{}\" Set when embedded as a populated relationship",
        col.slug
    );

    if col.timestamps {
        w!(out, "---@field created_at? string");
        w!(out, "---@field updated_at? string");
    }

    out.push('\n');
}

/// The `locale = "all"` read classes of a collection holding a per-locale
/// column: `crap.doc_localized.*` (each localized field a `{ [locale] =
/// value }` table, each group holding one its `crap.doc_group_localized.*`),
/// `crap.find_result_localized.*`, and `crap.query_all_locales.*` — the query
/// that returns them. Array and blocks rows read in the default locale, as
/// in `crap.doc.*`.
fn render_localized_classes(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    let read_fields = read_shape_fields(col);

    if !has_localized_columns(&read_fields, false) {
        return;
    }

    let shape = LuaShape::Localized { inherited: false };
    render_localized_group_classes(out, &read_fields, pascal, shape);

    w!(out, "---@class crap.doc_localized.{pascal} : crap.Document");
    w!(out, "---@field id string");
    write_fields(out, &read_fields, pascal, shape);
    write_doc_keys(out, col);

    w!(
        out,
        "---@class crap.find_result_localized.{pascal} : crap.FindResult"
    );
    w!(out, "---@field documents crap.doc_localized.{pascal}[]");
    out.push('\n');

    w!(
        out,
        "---@class crap.query_all_locales.{pascal} : crap.query.{pascal}"
    );
    w!(out, "---@field locale \"all\" Every locale at once");
    out.push('\n');
}

/// `crap.data.*` — hook `ctx.data`: the stored fields (the write shape's
/// field set — a virtual `Join` is never stored, at any depth — so every group
/// it names has the `crap.group.*` class rendered from the same set), EVERY one
/// optional.
/// An update's before-hooks see only the fields the request sends (a partial
/// update of `title` carries no other field, required or not), so no key can
/// be promised. `id` and timestamps are optional for the same reason: the
/// table is reused across hooks where they may or may not be populated (e.g.
/// `before_validate` on create has no id yet; `after_change` does).
fn render_data_class(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    w!(out, "---@class crap.data.{pascal}");
    w!(out, "---@field id? string");
    write_fields(
        out,
        &writable_fields(&col.fields),
        pascal,
        LuaShape::Partial,
    );
    if col.timestamps {
        w!(out, "---@field created_at? string");
        w!(out, "---@field updated_at? string");
    }
    out.push('\n');
}

/// The typed hook contexts: `crap.hook.*` (the write and `before_read`
/// events, `ctx.data` the stored shape) and `crap.read_hook.*` (`after_read`,
/// whose `ctx.data` is the document as the read returns it), plus the
/// `crap.find_result.*` wrapper.
fn render_hook_classes(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    w!(out, "---@class crap.hook.{pascal}");
    w!(out, "---@field collection \"{}\"", col.slug);
    w!(
        out,
        "---@field operation {}",
        literal_union(&collection_hook_operations())
    );
    w!(out, "---@field data crap.data.{pascal}");
    write_hook_context_tail(out);

    w!(out, "---@class crap.read_hook.{pascal}");
    w!(out, "---@field collection \"{}\"", col.slug);
    w!(
        out,
        "---@field operation {}",
        literal_union(&collection_read_hook_operations())
    );
    w!(out, "---@field data crap.doc.{pascal}");
    write_hook_context_tail(out);

    // crap.find_result.* — MUST extend `crap.FindResult` (mirrors
    // `crap.doc.X : crap.Document`). Empirically, LuaLS narrows
    // `---@overload` returns reliably when every variant shares a
    // common parent — `find_by_id` narrows because every variant
    // returns `crap.doc.X : crap.Document`; `find` only narrows once
    // every variant returns `crap.find_result.X : crap.FindResult`.
    // Without inheritance, LuaLS treats the variants as ad-hoc
    // siblings and unions them at the call site.
    w!(out, "---@class crap.find_result.{pascal} : crap.FindResult");
    w!(out, "---@field documents crap.doc.{pascal}[]");
    out.push('\n');
}

/// The one-liner `---@type` function aliases for the collection's hooks and
/// display conditions.
fn render_fn_aliases(out: &mut String, pascal: &str) {
    w!(
        out,
        "---@alias crap.hook_fn.{pascal} fun(ctx: crap.hook.{pascal}): crap.hook.{pascal}"
    );
    w!(
        out,
        "---@alias crap.read_hook_fn.{pascal} fun(ctx: crap.read_hook.{pascal}): crap.read_hook.{pascal}"
    );

    // Field-hook function alias — typed per-collection so field hooks
    // can replace `@param value any` + `@param context …` + `@return`
    // with a single `---@type crap.field_hook_fn.<Pascal>` cast.
    w!(
        out,
        "---@alias crap.field_hook_fn.{pascal} fun(value: any, context: crap.field_hook.{pascal}): any"
    );

    // Display-condition function alias — returns either a boolean
    // (server-evaluated) or a condition table (client-evaluated).
    // See `docs/src/admin-ui/guides/display-conditions.md`.
    w!(
        out,
        "---@alias crap.display_condition_fn.{pascal} fun(data: crap.data.{pascal}, ctx: crap.ConditionContext): boolean | table"
    );
    out.push('\n');
}

/// Render type definitions for a single collection.
pub(super) fn render_collection(out: &mut String, col: &CollectionDefinition) {
    let pascal = to_pascal_case(&col.slug);

    // Three families of sub-type classes: the stored write shape a write and
    // a hook's `ctx.data` hold (`crap.array_row.*` / `crap.group.*` — a
    // virtual `Join` is never stored, at any depth), its partial-write groups
    // (`crap.group_partial.*`), and the read shape a returned document
    // carries (`crap.doc_row.*` / `crap.doc_group.*`).
    let stored = writable_fields(&col.fields);
    render_sub_type_classes(out, &stored, &pascal, LuaShape::Input);
    render_partial_group_classes(out, &stored, &pascal);
    render_sub_type_classes(out, &read_shape_fields(col), &pascal, LuaShape::Read);

    render_data_class(out, col, &pascal);
    render_input_classes(out, col, &pascal);
    render_doc_class(out, col, &pascal);
    render_localized_classes(out, col, &pascal);
    render_hook_classes(out, col, &pascal);
    render_fn_aliases(out, &pascal);

    // crap.field_hook.* — the typed `crap.FieldHookContext`.
    w!(out, "---@class crap.field_hook.{pascal}");
    w!(out, "---@field collection \"{}\"", col.slug);
    write_field_hook_context(out, &format!("crap.data.{pascal}"));

    render_query_classes(out, col, &pascal);
    render_collection_accessor(out, col, &pascal);
    render_collection_typing_factories(out, col, &pascal);
}

#[cfg(test)]
mod tests;
