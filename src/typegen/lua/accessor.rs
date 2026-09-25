//! Per-collection and per-global accessor stubs (`crap.collections.<slug>`,
//! `crap.globals.<slug>`) and their pass-through typing factories.

use crate::{
    core::{
        CollectionDefinition, FieldType,
        collection::GlobalDefinition,
        upload::{read_shape_fields, readable_fields},
    },
    typegen::{
        helpers::{has_localized_columns, w, wraw},
        idents::lua_index,
    },
};

use super::field::{LuaShape, field_to_lua_type};

/// Field types that get a per-field `field_hook` overload in the generated Lua:
/// the leaf/scalar fields — everything that isn't a repeatable composite
/// (`Array`/`Blocks`), a layout wrapper (`Row`/`Collapsible`/`Tabs`), a `Group`,
/// or the virtual `Join`. Shared by the collection and global hook renderers so
/// the two can't drift on which fields are hookable.
fn takes_field_hook(ft: &FieldType) -> bool {
    !ft.has_rows() && !ft.is_layout_wrapper() && !matches!(ft, FieldType::Group | FieldType::Join)
}

/// The file-local table a collection's accessor stubs are declared on, bound
/// to `crap.collections.<slug>` at the end of the accessor. The typing
/// factories are declared on it too: `function crap.collections.<slug>.x()`
/// is a syntax error for a slug that isn't a Lua identifier (`2fa`, `end`),
/// while `_coll_<slug>` always is one (a slug is lowercase letters, digits
/// and underscores).
fn collection_local(slug: &str) -> String {
    format!("_coll_{slug}")
}

/// The file-local table a global's accessor stubs and typing factories are
/// declared on — see [`collection_local`].
fn global_local(slug: &str) -> String {
    format!("_glob_{slug}")
}

/// Pass-through typing factories on per-collection accessors:
///
/// - `crap.collections.<slug>.hook(fn)` — collection hook, typed ctx.
/// - `crap.collections.<slug>.read_hook(fn)` — `after_read` hook, ctx typed
///   with the read-shape document.
/// - `crap.collections.<slug>.field_hook(fn)` — any-field, typed ctx.
/// - `crap.collections.<slug>.field_hook(field, fn)` — per-field
///   narrowing of `value` from the field-name literal.
/// - `crap.collections.<slug>.condition(fn)` — display condition,
///   typed data.
///
/// Each is a runtime no-op (`f(fn) = fn`); the value of these
/// factories is that `LuaLS`'s `Lua.type.inferParamType` propagates
/// `value`/`ctx`/`data` types into the function literal's body.
pub(super) fn render_collection_typing_factories(
    out: &mut String,
    col: &CollectionDefinition,
    pascal: &str,
) {
    let slug = &col.slug;
    let local = collection_local(slug);

    // Each block consolidated into one `format!` to keep the function
    // under the `too_many_lines` clippy threshold and to make the Lua
    // output legible at the source level. Multi-line string literals
    // preserve their content verbatim, so what's written here is what
    // ends up in `hooks.lua`.

    wraw!(
        out,
        "--- Define a lifecycle hook for the `{slug}` collection (`before_validate`,
--- `before_change`, `after_change`, `before_read` — listed in the collection's
--- `hooks` table). The wrapper is a runtime no-op; its purpose is to narrow
--- `ctx` to `crap.hook.{pascal}` so `ctx.data.<field>` autocompletes.
---@param fn crap.hook_fn.{pascal}
---@return crap.hook_fn.{pascal}
function {local}.hook(fn) end

--- Define an `after_read` hook for the `{slug}` collection. Runtime no-op;
--- LuaLS narrows `ctx` to `crap.read_hook.{pascal}`, whose `ctx.data` is
--- the document as the read returns it (`crap.doc.{pascal}`).
---@param fn crap.read_hook_fn.{pascal}
---@return crap.read_hook_fn.{pascal}
function {local}.read_hook(fn) end

"
    );

    // field_hook(field, fn) — per-field overloads + any-field + dynamic-slug fallback.
    for f in &col.fields {
        if !takes_field_hook(&f.field_type) {
            continue;
        }
        let value_type = field_to_lua_type(f, pascal, LuaShape::Input);
        w!(
            out,
            "---@overload fun(field: \"{field}\", fn: fun(value: {value_type}, ctx: crap.field_hook.{pascal}): {value_type}?)",
            field = f.name,
        );
    }
    // Doc-prose comment lives below the overloads — LuaLS attaches the
    // prose adjacent to the `function ... end` decl to ALL overloads.
    wraw!(
        out,
        "---@overload fun(field: string, fn: crap.field_hook_fn.{pascal})
---@overload fun(fn: crap.field_hook_fn.{pascal})
--- Define a field hook for the `{slug}` collection. Two-arg form
--- (`field_hook(\"<field>\", fn)`) narrows `value` to the typed field;
--- single-arg form (`field_hook(fn)`) leaves `value` as `any`.
--- Use for value transformation (e.g. slug normalization, computed
--- defaults). Runtime no-op; LuaLS uses the signature to infer `value`
--- and `ctx: crap.field_hook.{pascal}`.
function {local}.field_hook(field, fn) end

"
    );

    wraw!(
        out,
        "--- Display condition for an admin-UI field on the `{slug}` collection.
--- Return `true`/`false` for visible/hidden, or a table
--- `{{ visible = bool, error = string? }}` for richer control. Runtime
--- no-op; LuaLS narrows `data` to `crap.data.{pascal}`.
---@param fn crap.display_condition_fn.{pascal}
---@return crap.display_condition_fn.{pascal}
function {local}.condition(fn) end

"
    );

    // access(fn) / auth_strategy(fn) — discoverable aliases for
    // `crap.any.access` / `crap.any.auth_strategy`. Context types are
    // uniform across collections so no per-collection narrowing is
    // possible, but surfacing them on the collection accessor makes
    // them findable via `crap.collections.<slug>.<TAB>` instead of
    // requiring users to know about the `crap.any` namespace upfront.
    wraw!(
        out,
        "--- Access-control function for `{slug}` operations. Return `true`/`false`
--- for global allow/deny, or a filter table to constrain results.
--- Runtime no-op; context type (`crap.AccessContext`) is uniform across
--- collections, so this is just a discoverable alias for `crap.any.access`.
---@param fn crap.access_fn
---@return crap.access_fn
function {local}.access(fn) end

--- Custom auth-strategy `authenticate` callback for the `{slug}` collection.
--- Receives request headers + the slug; returns a user document on
--- success or `nil` to fall through. Runtime no-op; discoverable alias
--- for `crap.any.auth_strategy`.
---@param fn crap.auth_strategy_fn
---@return crap.auth_strategy_fn
function {local}.auth_strategy(fn) end

--- Compute a display label for an array/blocks row on the `{slug}`
--- collection. Receives the row table and returns a string (or `nil`
--- to fall back to `label_field`). Runtime no-op; discoverable alias
--- for `crap.any.row_label`.
---@param fn crap.row_label_fn
---@return crap.row_label_fn
function {local}.row_label(fn) end

"
    );
}

/// Render the per-collection accessor at `crap.collections.<slug>`.
/// Each method has the slug closed over at runtime (registered in
/// `hooks/init.rs`), so user code calls `crap.collections.inquiries
/// .find({...})` instead of `crap.collections.find("inquiries", {...})`.
/// The single-collection signature also avoids the `---@overload`
/// narrowing limitation that forced the parent API to take a
/// uniform `crap.FindQuery` — here the per-collection `crap.query.X`
/// can be the parameter type directly, so inline `where = { ... }`
/// tables get per-column autocomplete.
pub(super) fn render_collection_accessor(
    out: &mut String,
    col: &CollectionDefinition,
    pascal: &str,
) {
    let slug: &str = &col.slug;
    let local = collection_local(slug);
    let variants = AccessorVariants::of(col);

    w!(out, "---@class crap.collections.{pascal}");
    w!(out, "local {local} = {{}}");
    out.push('\n');

    render_read_methods(out, &local, pascal, variants);

    // create — the write shape, required fields required; a draft save
    // skips the required checks, so it takes the all-optional payload
    w!(out, "---@param data crap.input.{pascal}");
    w!(out, "---@param opts? crap.CreateOptions");
    w!(out, "---@return crap.doc.{pascal}");
    if variants.drafts {
        w!(
            out,
            "---@overload fun(data: crap.partial.{pascal}, opts: crap.DraftCreateOptions): crap.doc.{pascal}"
        );
    }
    w!(out, "function {local}.create(data, opts) end");
    out.push('\n');

    // update — partial payload (only the fields being changed)
    w!(out, "---@param id string");
    w!(out, "---@param data crap.partial.{pascal}");
    w!(out, "---@param opts? crap.UpdateOptions");
    w!(out, "---@return crap.doc.{pascal}");
    w!(out, "function {local}.update(id, data, opts) end");
    out.push('\n');

    // delete
    w!(out, "---@param id string");
    w!(out, "---@param opts? crap.DeleteOptions");
    w!(out, "---@return boolean");
    w!(out, "function {local}.delete(id, opts) end");
    out.push('\n');

    // unpublish
    w!(out, "---@param id string");
    w!(out, "---@param opts? crap.UnpublishOptions");
    w!(out, "---@return crap.doc.{pascal}");
    w!(out, "function {local}.unpublish(id, opts) end");
    out.push('\n');

    // undelete
    w!(out, "---@param id string");
    w!(out, "---@param opts? crap.UndeleteOptions");
    w!(out, "---@return boolean");
    w!(out, "function {local}.undelete(id, opts) end");
    out.push('\n');

    // validate — a dry-run create (a draft one relaxes required fields)
    w!(out, "---@param data crap.input.{pascal}");
    w!(out, "---@param opts? crap.ValidateOptions");
    w!(out, "---@return crap.ValidateResult");
    if variants.drafts {
        w!(
            out,
            "---@overload fun(data: crap.partial.{pascal}, opts: crap.DraftValidateOptions): crap.ValidateResult"
        );
    }
    w!(out, "function {local}.validate(data, opts) end");
    out.push('\n');

    // count
    w!(out, "---@param query? crap.CountQuery");
    w!(out, "---@return integer");
    w!(out, "function {local}.count(query) end");
    out.push('\n');

    // create_many
    w!(out, "---@param items crap.input.{pascal}[]");
    w!(out, "---@param opts? crap.CreateOptions");
    w!(out, "---@return crap.CreateManyResult");
    if variants.drafts {
        w!(
            out,
            "---@overload fun(items: crap.partial.{pascal}[], opts: crap.DraftCreateOptions): crap.CreateManyResult"
        );
    }
    w!(out, "function {local}.create_many(items, opts) end");
    out.push('\n');

    // update_many — partial payload applied to every matched doc; never a
    // password (refused in bulk)
    w!(out, "---@param query crap.UpdateManyQuery");
    w!(out, "---@param data crap.partial_many.{pascal}");
    w!(out, "---@param opts? crap.UpdateOptions");
    w!(out, "---@return crap.UpdateManyResult");
    w!(out, "function {local}.update_many(query, data, opts) end");
    out.push('\n');

    // delete_many
    w!(out, "---@param query crap.DeleteManyQuery");
    w!(out, "---@param opts? crap.DeleteOptions");
    w!(out, "---@return crap.DeleteManyResult");
    w!(out, "function {local}.delete_many(query, opts) end");
    out.push('\n');

    // list_versions
    w!(out, "---@param id string");
    w!(out, "---@param opts? crap.ListVersionsOptions");
    w!(out, "---@return crap.ListVersionsResult");
    w!(out, "function {local}.list_versions(id, opts) end");
    out.push('\n');

    // restore_version
    w!(out, "---@param id string");
    w!(out, "---@param version_id string");
    w!(out, "---@param opts? crap.RestoreVersionOptions");
    w!(out, "---@return crap.doc.{pascal}");
    w!(
        out,
        "function {local}.restore_version(id, version_id, opts) end"
    );
    out.push('\n');

    // ref_count
    w!(out, "---@param id string");
    w!(out, "---@return integer");
    w!(out, "function {local}.ref_count(id) end");
    out.push('\n');

    w!(out, "{} = {local}", lua_index("crap.collections", slug));
    out.push('\n');
}

/// Which signature variants a collection's accessor declares beside the
/// plain ones.
#[derive(Clone, Copy)]
struct AccessorVariants {
    /// Drafts are enabled: a `draft = true` create / validate skips the
    /// required checks.
    drafts: bool,
    /// A field reads per locale: a `locale = "all"` read returns
    /// `crap.doc_localized.*`.
    all_locales: bool,
}

impl AccessorVariants {
    fn of(col: &CollectionDefinition) -> Self {
        Self {
            drafts: col.has_drafts(),
            all_locales: has_localized_columns(&read_shape_fields(col), false),
        }
    }
}

/// `find` and `find_by_id`, each with its `locale = "all"` overload when a
/// field reads per locale.
fn render_read_methods(out: &mut String, local: &str, pascal: &str, variants: AccessorVariants) {
    w!(out, "---@param query? crap.query.{pascal}");
    w!(out, "---@return crap.find_result.{pascal}");
    if variants.all_locales {
        w!(
            out,
            "---@overload fun(query: crap.query_all_locales.{pascal}): crap.find_result_localized.{pascal}"
        );
    }
    w!(out, "function {local}.find(query) end");
    out.push('\n');

    w!(out, "---@param id string");
    w!(out, "---@param opts? crap.FindByIdOptions");
    w!(out, "---@return crap.doc.{pascal}?");
    if variants.all_locales {
        w!(
            out,
            "---@overload fun(id: string, opts: crap.AllLocalesFindByIdOptions): crap.doc_localized.{pascal}?"
        );
    }
    w!(out, "function {local}.find_by_id(id, opts) end");
    out.push('\n');
}

/// Pass-through typing factories on per-global accessors. Mirrors
/// the per-collection variant — `hook`, `read_hook`, `field_hook(field?, fn)`,
/// `condition` — all runtime no-ops, all giving `LuaLS` a typed
/// parameter slot for inference.
pub(super) fn render_global_typing_factories(
    out: &mut String,
    global: &GlobalDefinition,
    pascal: &str,
) {
    let slug = &global.slug;
    let local = global_local(slug);

    // Same consolidation as the per-collection variant — see
    // `render_collection_typing_factories` for the rationale.

    wraw!(
        out,
        "--- Define a lifecycle hook for the `{slug}` global (`before_validate`,
--- `before_change`, `after_change`, `before_read`). Runtime no-op; LuaLS narrows
--- `ctx` to `crap.hook.global_{slug}` so `ctx.data.<field>` autocompletes.
---@param fn crap.hook_fn.global_{slug}
---@return crap.hook_fn.global_{slug}
function {local}.hook(fn) end

--- Define an `after_read` hook for the `{slug}` global. Runtime no-op;
--- LuaLS narrows `ctx` to `crap.read_hook.global_{slug}`, whose `ctx.data`
--- is the document as the read returns it (`crap.global_doc.{pascal}`).
---@param fn crap.read_hook_fn.global_{slug}
---@return crap.read_hook_fn.global_{slug}
function {local}.read_hook(fn) end

"
    );

    // field_hook(field, fn) — per-field overloads + any-field form.
    for f in &global.fields {
        if !takes_field_hook(&f.field_type) {
            continue;
        }
        let value_type = field_to_lua_type(f, pascal, LuaShape::Input);
        w!(
            out,
            "---@overload fun(field: \"{field}\", fn: fun(value: {value_type}, ctx: crap.field_hook.global_{slug}): {value_type}?)",
            field = f.name,
        );
    }
    wraw!(
        out,
        "---@overload fun(field: string, fn: crap.field_hook_fn.global_{slug})
---@overload fun(fn: crap.field_hook_fn.global_{slug})
--- Define a field hook for the `{slug}` global. Two-arg form narrows
--- `value` to the typed field; single-arg form leaves it as `any`.
--- Runtime no-op; LuaLS uses the signature to infer `value` and
--- `ctx: crap.field_hook.global_{slug}`.
function {local}.field_hook(field, fn) end

--- Display condition for an admin-UI field on the `{slug}` global.
--- Return `true`/`false`, or a `{{ visible, error }}` table. Runtime
--- no-op; LuaLS narrows `data` to `crap.global_data.{pascal}`.
---@param fn crap.display_condition_fn.global_{slug}
---@return crap.display_condition_fn.global_{slug}
function {local}.condition(fn) end

--- Access-control function for `{slug}` global operations. Return
--- `true`/`false` or a filter table. Runtime no-op; discoverable alias
--- for `crap.any.access`.
---@param fn crap.access_fn
---@return crap.access_fn
function {local}.access(fn) end

--- Compute a display label for an array/blocks row on the `{slug}`
--- global. Returns a string (or `nil` to fall back to `label_field`).
--- Runtime no-op; discoverable alias for `crap.any.row_label`.
---@param fn crap.row_label_fn
---@return crap.row_label_fn
function {local}.row_label(fn) end

"
    );
}

/// Per-global accessor at `crap.globals.<slug>`. Slug-less
/// `get(opts?)` / `update(data, opts?)` / `unpublish(opts?)` /
/// `validate(data, opts?)` with typed returns — no overload
/// narrowing concerns. Runtime-registered in `hooks/init.rs` to
/// dispatch to the slug-keyed `crap.globals.*` functions.
pub(super) fn render_global_accessor(out: &mut String, global: &GlobalDefinition, pascal: &str) {
    let slug: &str = &global.slug;
    let local = global_local(slug);
    w!(out, "---@class crap.globals.{pascal}");
    w!(out, "local {local} = {{}}");
    out.push('\n');

    // get — with its `locale = "all"` overload when a field reads per locale
    w!(out, "---@param opts? crap.GlobalGetOptions");
    w!(out, "---@return crap.global_doc.{pascal}");
    if has_localized_columns(&readable_fields(&global.fields), false) {
        w!(
            out,
            "---@overload fun(opts: crap.AllLocalesGlobalGetOptions): crap.global_doc_localized.{pascal}"
        );
    }
    w!(out, "function {local}.get(opts) end");
    out.push('\n');

    // update — partial payload
    w!(out, "---@param data crap.global_partial.{pascal}");
    w!(out, "---@param opts? crap.GlobalUpdateOptions");
    w!(out, "---@return crap.global_doc.{pascal}");
    w!(out, "function {local}.update(data, opts) end");
    out.push('\n');

    // unpublish — versioned globals only (runtime-errors otherwise)
    w!(out, "---@param opts? crap.GlobalUnpublishOptions");
    w!(out, "---@return crap.global_doc.{pascal}");
    w!(out, "function {local}.unpublish(opts) end");
    out.push('\n');

    // validate — dry-run against the singleton row
    w!(out, "---@param data crap.global_partial.{pascal}");
    w!(out, "---@param opts? crap.GlobalValidateOptions");
    w!(out, "---@return crap.ValidateResult");
    w!(out, "function {local}.validate(data, opts) end");
    out.push('\n');

    w!(out, "{} = {local}", lua_index("crap.globals", slug));
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldDefinition, VersionsConfig};

    /// One accessor stub: its annotations, up to the `function` line.
    fn stub<'a>(out: &'a str, function: &str) -> &'a str {
        let end = out
            .find(function)
            .unwrap_or_else(|| panic!("{function} not emitted:\n{out}"));
        let start = out[..end].rfind("\n\n").map_or(0, |i| i + 2);

        &out[start..end]
    }

    /// Regression: `create`, `create_many` and `validate` took the hook-data
    /// class, which declares the server-derived upload columns and the
    /// virtual joins no write accepts. They take the write-shape input class;
    /// `update` takes the all-optional partial class.
    #[test]
    fn write_methods_take_the_write_shape_classes() {
        let mut out = String::new();
        render_collection_accessor(&mut out, &CollectionDefinition::new("posts"), "Posts");

        let create = stub(&out, "function _coll_posts.create(data, opts) end");
        assert!(
            create.contains("---@param data crap.input.Posts"),
            "{create}"
        );

        let many = stub(&out, "function _coll_posts.create_many(items, opts) end");
        assert!(
            many.contains("---@param items crap.input.Posts[]"),
            "{many}"
        );

        let validate = stub(&out, "function _coll_posts.validate(data, opts) end");
        assert!(
            validate.contains("---@param data crap.input.Posts"),
            "{validate}"
        );

        let update = stub(&out, "function _coll_posts.update(id, data, opts) end");
        assert!(
            update.contains("---@param data crap.partial.Posts"),
            "{update}"
        );

        // `update_many` refuses a password, so it takes the bulk payload class
        // that has none.
        let update_many = stub(
            &out,
            "function _coll_posts.update_many(query, data, opts) end",
        );
        assert!(
            update_many.contains("---@param data crap.partial_many.Posts"),
            "{update_many}"
        );

        assert!(!out.contains("crap.data.Posts"), "{out}");
    }

    /// Regression: a slug that isn't a Lua identifier was bound dotted
    /// (`crap.collections.2fa = …`, `crap.globals.end = …`), a syntax error.
    /// It is bound with a quoted index, so `crap.collections["2fa"]` is typed.
    #[test]
    fn non_identifier_slugs_bind_with_a_quoted_index() {
        let mut out = String::new();
        render_collection_accessor(&mut out, &CollectionDefinition::new("2fa"), "2fa");
        assert!(
            out.contains("crap.collections[\"2fa\"] = _coll_2fa\n"),
            "{out}"
        );

        let mut out = String::new();
        render_global_accessor(&mut out, &GlobalDefinition::new("end"), "End");
        assert!(out.contains("crap.globals[\"end\"] = _glob_end\n"), "{out}");

        let mut out = String::new();
        render_global_accessor(&mut out, &GlobalDefinition::new("settings"), "Settings");
        assert!(
            out.contains("crap.globals.settings = _glob_settings\n"),
            "{out}"
        );
    }

    /// Regression: the typing factories were declared as
    /// `function crap.collections.<slug>.hook(fn) end` — a syntax error for a
    /// slug like `2fa`, and for every slug a field `LuaLS` refuses to inject
    /// into the accessor class (`inject-field`). They are declared on the
    /// class-bound local, like the accessor methods.
    #[test]
    fn typing_factories_are_declared_on_the_accessor_local() {
        let col = CollectionDefinition::new("2fa");
        let mut out = String::new();
        render_collection_typing_factories(&mut out, &col, "2fa");

        for factory in [
            "function _coll_2fa.hook(fn) end",
            "function _coll_2fa.read_hook(fn) end",
            "function _coll_2fa.field_hook(field, fn) end",
            "function _coll_2fa.condition(fn) end",
            "function _coll_2fa.access(fn) end",
            "function _coll_2fa.auth_strategy(fn) end",
            "function _coll_2fa.row_label(fn) end",
        ] {
            assert!(out.contains(factory), "{factory}: {out}");
        }
        assert!(!out.contains("function crap."), "{out}");

        let global = GlobalDefinition::new("end");
        let mut out = String::new();
        render_global_typing_factories(&mut out, &global, "End");

        for factory in [
            "function _glob_end.hook(fn) end",
            "function _glob_end.read_hook(fn) end",
            "function _glob_end.field_hook(field, fn) end",
            "function _glob_end.condition(fn) end",
            "function _glob_end.access(fn) end",
            "function _glob_end.row_label(fn) end",
        ] {
            assert!(out.contains(factory), "{factory}: {out}");
        }
        assert!(!out.contains("function crap."), "{out}");
    }

    /// Regression: a draft create was typed as a full create, so the
    /// documented `create({ ... }, { draft = true })` flow with required
    /// fields left out was a `missing-fields` diagnostic. A drafts collection
    /// declares the draft overloads; one without drafts does not.
    #[test]
    fn a_drafts_collection_declares_the_draft_write_overloads() {
        let mut col = CollectionDefinition::new("posts");
        col.versions = Some(VersionsConfig::new(true, 0));

        let mut out = String::new();
        render_collection_accessor(&mut out, &col, "Posts");

        for (function, overload) in [
            (
                "function _coll_posts.create(data, opts) end",
                "---@overload fun(data: crap.partial.Posts, opts: crap.DraftCreateOptions): crap.doc.Posts",
            ),
            (
                "function _coll_posts.create_many(items, opts) end",
                "---@overload fun(items: crap.partial.Posts[], opts: crap.DraftCreateOptions): crap.CreateManyResult",
            ),
            (
                "function _coll_posts.validate(data, opts) end",
                "---@overload fun(data: crap.partial.Posts, opts: crap.DraftValidateOptions): crap.ValidateResult",
            ),
        ] {
            assert!(stub(&out, function).contains(overload), "{overload}: {out}");
        }

        let mut out = String::new();
        render_collection_accessor(&mut out, &CollectionDefinition::new("tags"), "Tags");
        assert!(!out.contains("Draft"), "{out}");
    }

    /// Regression: a `locale = "all"` read was typed as a single-locale
    /// document. A collection or global holding a localized field declares
    /// the all-locales overloads returning the per-locale classes.
    #[test]
    fn localized_owners_declare_the_all_locales_overloads() {
        let mut col = CollectionDefinition::new("posts");
        col.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
        ];

        let mut out = String::new();
        render_collection_accessor(&mut out, &col, "Posts");

        assert!(
            stub(&out, "function _coll_posts.find(query) end").contains(
                "---@overload fun(query: crap.query_all_locales.Posts): crap.find_result_localized.Posts"
            ),
            "{out}"
        );
        assert!(
            stub(&out, "function _coll_posts.find_by_id(id, opts) end").contains(
                "---@overload fun(id: string, opts: crap.AllLocalesFindByIdOptions): crap.doc_localized.Posts?"
            ),
            "{out}"
        );

        let mut global = GlobalDefinition::new("settings");
        global.fields = col.fields.clone();

        let mut out = String::new();
        render_global_accessor(&mut out, &global, "Settings");
        assert!(
            stub(&out, "function _glob_settings.get(opts) end").contains(
                "---@overload fun(opts: crap.AllLocalesGlobalGetOptions): crap.global_doc_localized.Settings"
            ),
            "{out}"
        );

        let mut out = String::new();
        render_collection_accessor(&mut out, &CollectionDefinition::new("tags"), "Tags");
        assert!(
            !out.contains("all_locales") && !out.contains("AllLocales"),
            "{out}"
        );
    }
}
