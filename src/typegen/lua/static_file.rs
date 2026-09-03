//! Assemble the static `types/crap.lua` from Rust source.
//!
//! Composed in [`render_static_file`] by calling each block-renderer in
//! [`BLOCK_RENDERS`] in order. Each renderer appends one section to a
//! shared output buffer. Every class / alias / function block is
//! emitted by a derive (`LuaAnnotation`, `LuaAlias`, `LuaTypeAlias`,
//! `LuaFieldTypeViews`) or a `lua_table!`-generated `render_X_lua`
//! fn — there are no hand-written `.lua` block files left in the
//! pipeline. The fixed `---@meta` banner at the top of the output
//! is the only literal string content, declared inline as
//! [`HEADER`] for visibility. Bare `---@meta` (no module name) marks
//! the file as a stub: `LuaLS` treats every declaration as a type-only
//! definition and indexes it once even when the file appears in both
//! `Lua.workspace.library` and the workspace root.

use super::{LuaAlias, LuaAnnotation, LuaFieldTypeViews};

use crate::core::{
    BlockDefinition, Document, FieldAccess, FieldAdmin, FieldAdminLabels, FieldDefinition,
    FieldHooks, FieldTab, FieldType, HookRef, Hooks, IndexDefinition, Labels, LocalizedString,
    McpFieldConfig, RelationshipConfig, SelectOption, VersionsConfig,
    collection::{
        Access, Activation, AdminConfig, Auth, AuthMethod, CollectionDefinition, GlobalAccess,
        GlobalDefinition, McpConfig, Surface,
    },
    field::{FieldHookFn, FieldWidth, JoinConfig, PickerAppearance, ValidateFunction},
    job::JobLabels,
    upload::{CollectionUpload, FormatOptions, FormatQuality, ImageFit, ImageSize},
};
use crate::db::query::PaginationResult;
use crate::hooks::lifecycle::{
    AccessContext, AuthStrategyContext, ConditionContext, FieldHookContext, HookContext, HookEvent,
    JobHandlerContext, JobInfo, LiveFilterContext, MfaDeliverContext, MfaWhenContext, RouteContext,
    ValidateContext,
};
use crate::hooks::lua_api::{
    access::render_crap_access_init_lua,
    auth::render_crap_auth_lua,
    collections::{render_crap_collections_init_config_lua, render_crap_collections_init_root_lua},
    config::{render_crap_config_lua, render_crap_locale_lua},
    crud::{
        collection::{
            bulk::{
                create_many::{CreateManyOpts, render_crap_collections_create_many_lua},
                delete_many::{
                    DeleteManyOpts, DeleteManyQueryInput, DeleteManyResult,
                    render_crap_collections_delete_many_lua,
                },
                update_many::{
                    UpdateManyOpts, UpdateManyQueryInput, render_crap_collections_update_many_lua,
                },
            },
            read::{
                count::{CountQueryInput, render_crap_collections_count_lua},
                find::{FindQueryInput, FindResult},
                find_by_id::FindByIdOptions,
            },
            ref_count::render_crap_collections_ref_count_lua,
            versions::{
                list::{
                    ListVersionsOptions, ListVersionsResult, VersionSummary,
                    render_crap_collections_list_versions_lua,
                },
                restore::{RestoreVersionOptions, render_crap_collections_restore_version_lua},
            },
            write::{
                create::{CreateOptions, render_crap_collections_create_lua},
                delete::{DeleteOptions, render_crap_collections_delete_lua},
                undelete::{UndeleteOptions, render_crap_collections_undelete_lua},
                unpublish::{UnpublishOptions, render_crap_collections_unpublish_lua},
                update::{UpdateOptions, render_crap_collections_update_lua},
                validate::{ValidateOptions, ValidateResult, render_crap_collections_validate_lua},
            },
        },
        filter::{FilterOperators, FilterScalar, FilterValue, OrCondition},
        globals::{
            get::{GlobalGetOptions, render_crap_globals_get_lua},
            unpublish::{GlobalUnpublishOptions, render_crap_globals_unpublish_lua},
            update::{GlobalUpdateOptions, render_crap_globals_update_lua},
            validate::{GlobalValidateOptions, render_crap_globals_validate_lua},
        },
        jobs::queue::render_crap_jobs_queue_lua,
    },
    crypto::{render_crap_crypto_stateful_lua, render_crap_crypto_stateless_lua},
    email::{EmailOptions, render_crap_email_lua},
    env::render_crap_env_lua,
    fields::render_crap_fields_lua,
    globals::{render_crap_globals_init_config_lua, render_crap_globals_init_root_lua},
    hooks::render_crap_hooks_lua,
    http::{HttpRequest, HttpResponse, render_crap_http_lua},
    jobs::render_crap_jobs_init_lua,
    log::render_crap_log_lua,
    pages::{PageOptions, render_crap_pages_lua},
    parse::JobDefinitionConfig,
    richtext::{RichtextNodeSpec, render_crap_richtext_init_lua, render_crap_richtext_render_lua},
    routes::render_crap_routes_lua,
    schema::{SchemaCollection, SchemaField, render_crap_schema_init_lua},
    storage::render_crap_storage_lua,
    template_data::render_crap_template_data_lua,
    transaction::render_crap_transaction_lua,
    tx_hooks::render_crap_tx_lua,
    uploads::render_crap_uploads_lua,
    utils::{render_crap_json_lua, render_crap_util_lua},
};
use crate::service::{CreateManyResult, UpdateManyResult};

/// Signature for a function that appends one section to the output buffer.
type BlockRender = fn(&mut String);

/// Ordered list of section renderers. Each entry contributes one
/// section to the assembled `types/crap.lua` output. Order matters —
/// it's the order sections appear in the final file.
const BLOCK_RENDERS: &[BlockRender] = &[
    render_header,
    render_field_types,
    render_field_factories,
    render_collection_types,
    render_global_types,
    render_document_types,
    render_hook_context_types,
    render_crap_collections,
    render_crap_globals,
    render_crap_hooks,
    render_crap_richtext,
    render_crap_log,
    render_crap_json,
    render_crap_util,
    render_crap_util_table_helpers,
    render_crap_util_string_helpers,
    render_crap_auth,
    render_crap_access,
    render_crap_env,
    render_crap_http,
    render_crap_email,
    render_crap_storage,
    render_crap_config,
    render_crap_locale,
    render_crap_crypto,
    render_crap_schema,
    render_crap_jobs,
    render_crap_pages,
    render_crap_routes,
    render_crap_template_data,
    render_crap_transaction,
    render_crap_tx,
    render_crap_uploads,
];

/// Render the complete static `types/crap.lua` file from Rust source.
#[must_use]
pub fn render_static_file() -> String {
    let mut out = String::with_capacity(80_000);
    for render in BLOCK_RENDERS {
        render(&mut out);
    }
    out
}

// ── Block renderers ──────────────────────────────────────────────────

/// `---@meta crap` banner + intro doc + global `crap` class. Fixed
/// content, no Rust counterpart — emitted verbatim once at the top of
/// the output.
const HEADER: &str = "\
---@meta
--- Crap CMS Lua API type definitions for lua-language-server (LuaLS).
---
--- This file is NOT executed at runtime. It provides type annotations
--- for IDE autocompletion, hover docs, and type checking.
---
--- Add this to your LuaLS workspace library or .luarc.json:
---   { \"Lua.workspace.library\": [\"./types\"] }

--- @class crap
--- The global `crap` table — entry point for all CMS operations.
--- Available in init.lua, collection definitions, and hook functions.
crap = {}


";

fn render_header(out: &mut String) {
    out.push_str(HEADER);
}
fn render_field_types(out: &mut String) {
    FieldType::render_lua_alias(out);
    LocalizedString::render_lua_alias(out);
    FieldWidth::render_lua_alias(out);
    RelationshipConfig::render_lua_annotation(out);
    SelectOption::render_lua_annotation(out);
    FieldAdmin::render_lua_annotation(out);
    ValidateFunction::render_lua_alias(out);
    ValidateContext::render_lua_annotation(out);
    FieldHooks::render_lua_annotation(out);
    FieldHookFn::render_lua_alias(out);
    FieldHookContext::render_lua_annotation(out);
    ConditionContext::render_lua_annotation(out);
    FieldTab::render_lua_annotation(out);
    BlockDefinition::render_lua_annotation(out);
    PickerAppearance::render_lua_alias(out);
    McpFieldConfig::render_lua_annotation(out);
    McpConfig::render_lua_annotation(out);
    FieldAdminLabels::render_lua_annotation(out);
}
fn render_field_factories(out: &mut String) {
    FieldDefinition::render_lua_field_type_views(out);
    FieldDefinition::render_lua_annotation(out);
    JoinConfig::render_lua_annotation(out);
    render_crap_fields_lua(out);
    out.push('\n');
}
fn render_collection_types(out: &mut String) {
    Labels::render_lua_annotation(out);
    AdminConfig::render_lua_annotation(out);
    HookRef::render_lua_annotation(out);
    Hooks::render_lua_annotation(out);
    Access::render_lua_annotation(out);
    AuthStrategyContext::render_lua_annotation(out);
    MfaWhenContext::render_lua_annotation(out);
    MfaDeliverContext::render_lua_annotation(out);
    LiveFilterContext::render_lua_annotation(out);
    Surface::render_lua_alias(out);
    Activation::render_lua_alias(out);
    AuthMethod::render_lua_alias(out);
    Auth::render_lua_annotation(out);

    ImageFit::render_lua_alias(out);
    ImageSize::render_lua_annotation(out);
    FormatQuality::render_lua_annotation(out);
    FormatOptions::render_lua_annotation(out);
    CollectionUpload::render_lua_annotation(out);
    VersionsConfig::render_lua_annotation(out);
    CollectionDefinition::render_lua_annotation(out);
    IndexDefinition::render_lua_annotation(out);
    out.push('\n');
}
fn render_global_types(out: &mut String) {
    GlobalAccess::render_lua_annotation(out);
    GlobalDefinition::render_lua_annotation(out);
}
fn render_document_types(out: &mut String) {
    PaginationResult::render_lua_annotation(out);
    Document::render_lua_annotation(out);
    FilterScalar::render_lua_alias(out);
    FilterOperators::render_lua_annotation(out);
    FilterValue::render_lua_alias(out);
    OrCondition::render_lua_alias(out);
    FindQueryInput::render_lua_annotation(out);
    FindResult::render_lua_annotation(out);
}
fn render_hook_context_types(out: &mut String) {
    FieldAccess::render_lua_annotation(out);
    HookContext::render_lua_annotation(out);
    render_auth_user(out);
    AccessContext::render_lua_annotation(out);
    render_callable_aliases(out);
    render_any_factories(out);
}

/// `crap.AuthUser` — the type of `crap.AccessContext.user`. Extends
/// `crap.Document` (so `id` / timestamps are typed) AND carries an
/// `[string] any` index signature so arbitrary field access works
/// (`context.user.role`, `context.user.email`) — necessary because
/// a project can have multiple auth collections and we can't
/// statically know which doc shape `user` will be.
///
/// Lives on a separate class (not on `crap.Document` itself)
/// specifically so the `[string] any` index doesn't leak into typed
/// subclasses like `crap.doc.X : crap.Document` — that's the bug
/// that collapsed typed-doc hovers in earlier iterations.
fn render_auth_user(out: &mut String) {
    out.push_str(
        "\
--- The authenticated user attached to `crap.AccessContext.user`.
--- Carries the base `crap.Document` shape (id + timestamps) plus an
--- index signature for arbitrary field access — the static type
--- can't narrow further because a project can have multiple auth
--- collections. Cast explicitly when you know the collection:
--- `local u = context.user --[[@as crap.doc.Users]]`.
--- @class crap.AuthUser : crap.Document
--- @field [string] any

",
    );
}

/// `crap.any.*` — cross-collection typing-helper factories. Each is
/// a pass-through at runtime (`f(fn) = fn`) whose only purpose is to
/// give `LuaLS` a typed parameter slot so `Lua.type.inferParamType =
/// true` propagates the right `ctx` / `value` types into the
/// function literal's body.
///
/// Pair this with the per-collection accessors
/// (`crap.collections.<slug>.{hook,field_hook,condition}`) which
/// give per-collection / per-field narrowing.
fn render_any_factories(out: &mut String) {
    out.push_str(
        "\
-- ── crap.any — cross-collection typing helpers ─────────────────────

--- Typing helpers for callables that don't belong to a specific
--- collection (generic-context collection/field hooks, access
--- functions, auth strategies, job handlers, row labels). Each
--- function is a no-op pass-through at runtime; its only purpose is
--- to give LuaLS a typed parameter slot so the body's `value` / `ctx`
--- params get inferred via `Lua.type.inferParamType`.
---
--- For per-collection narrowing use the slug accessors instead:
--- `crap.collections.<slug>.{hook,field_hook,condition}` /
--- `crap.globals.<slug>.{hook,field_hook,condition}`.
--- @class crap.any
crap.any = {}

--- Wrap a collection hook that takes a generic `crap.HookContext`.
--- @param fn crap.hook_fn
--- @return crap.hook_fn
function crap.any.collection_hook(fn) end

--- Wrap a field hook that takes a generic `crap.FieldHookContext`.
--- @param fn crap.field_hook_fn
--- @return crap.field_hook_fn
function crap.any.field_hook(fn) end

--- Wrap an access control function.
--- @param fn crap.access_fn
--- @return crap.access_fn
function crap.any.access(fn) end

--- Wrap an auth strategy `authenticate` callback.
--- @param fn crap.auth_strategy_fn
--- @return crap.auth_strategy_fn
function crap.any.auth_strategy(fn) end

--- Wrap a job handler.
--- @param fn crap.job_handler_fn
--- @return crap.job_handler_fn
function crap.any.job_handler(fn) end

--- Wrap a custom-route handler (so `ctx` types as `crap.RouteContext`).
--- @param fn crap.route_handler_fn
--- @return crap.route_handler_fn
function crap.any.route_handler(fn) end

--- Wrap a row-label function for arrays/blocks.
--- @param fn crap.row_label_fn
--- @return crap.row_label_fn
function crap.any.row_label(fn) end

--- Wrap a generic display condition (no per-collection data narrowing).
--- @param fn fun(data: table<string, any>, ctx: crap.ConditionContext): boolean | table
--- @return fun(data: table<string, any>, ctx: crap.ConditionContext): boolean | table
function crap.any.display_condition(fn) end

",
    );
}

/// Function-type aliases for every callable users define
/// (hooks, access, auth strategies, job handlers, row labels).
/// Each lets users replace 2–4 lines of `---@param` / `---@return`
/// annotations with a single `---@type crap.X_fn` cast.
///
/// Per-collection variants (`crap.field_hook_fn.<Pascal>`,
/// `crap.display_condition_fn.<Pascal>`) are emitted next to the
/// per-collection types in `render::render_collection` — the static
/// file only owns the slug-agnostic ones.
fn render_callable_aliases(out: &mut String) {
    out.push_str(
        "\
-- ── Function-type aliases (one-liner `---@type` for callables) ─────

--- Generic collection hook. Use for hooks that take `crap.HookContext`
--- (no per-collection narrowing); for typed contexts use
--- `crap.hook_fn.<Pascal>` from `hooks.lua`.
--- @alias crap.hook_fn fun(ctx: crap.HookContext): crap.HookContext

--- Generic field hook. Use for hooks that take the generic
--- `crap.FieldHookContext`; for typed contexts use
--- `crap.field_hook_fn.<Pascal>` from `hooks.lua`. Same shape
--- as the older `crap.FieldHookFn` alias — both are kept; new code
--- should prefer this lowercase name for naming consistency with
--- the per-collection variants.
--- @alias crap.field_hook_fn fun(value: any, context: crap.FieldHookContext): any

--- Access control function. Returns `true`/`false` for global allow /
--- deny, or a filter table to constrain results.
--- @alias crap.access_fn fun(ctx: crap.AccessContext): boolean | table

--- Custom auth strategy `authenticate` callback. Receives request
--- headers + the collection slug; returns a user document on success
--- or `nil` to fall through to the next method.
--- @alias crap.auth_strategy_fn fun(ctx: crap.AuthStrategyContext): crap.Document?
--- @alias crap.mfa_when_fn fun(ctx: crap.MfaWhenContext): boolean?
--- @alias crap.mfa_deliver_fn fun(ctx: crap.MfaDeliverContext)

--- Job handler entry point. The runtime ignores the return value.
--- @alias crap.job_handler_fn fun(ctx: crap.JobHandlerContext)

--- Computed row label for arrays/blocks. Receives the row table and
--- returns the display string (or `nil` to fall back to `label_field`).
--- @alias crap.row_label_fn fun(row: table<string, any>): string?

",
    );
}
fn render_crap_collections(out: &mut String) {
    render_crap_collections_init_root_lua(out);
    render_crap_collections_init_config_lua(out);
    // `find` and `find_by_id` are intentionally NOT emitted into the
    // static crap.lua: their typed signatures live exclusively in the
    // per-project `hooks.lua` so the per-collection
    // `---@overload`s can narrow the return type from the slug
    // literal. Declaring the same function in both files made LuaLS
    // merge the two return types into a union — every call site
    // showed `crap.FindResult | crap.find_result.Categories | ...`,
    // and iterating `result.documents` produced a union of every
    // per-collection `crap.doc.*[]`. Runtime registration is
    // unaffected; this only skips type-annotation emission.
    FindByIdOptions::render_lua_annotation(out);
    CreateOptions::render_lua_annotation(out);
    render_crap_collections_create_lua(out);
    UpdateOptions::render_lua_annotation(out);
    render_crap_collections_update_lua(out);
    DeleteOptions::render_lua_annotation(out);
    render_crap_collections_delete_lua(out);
    UnpublishOptions::render_lua_annotation(out);
    render_crap_collections_unpublish_lua(out);
    UndeleteOptions::render_lua_annotation(out);
    render_crap_collections_undelete_lua(out);
    ValidateOptions::render_lua_annotation(out);
    ValidateResult::render_lua_annotation(out);
    render_crap_collections_validate_lua(out);
    CountQueryInput::render_lua_annotation(out);
    render_crap_collections_count_lua(out);
    UpdateManyQueryInput::render_lua_annotation(out);
    DeleteManyQueryInput::render_lua_annotation(out);
    CreateManyOpts::render_lua_annotation(out);
    UpdateManyOpts::render_lua_annotation(out);
    DeleteManyOpts::render_lua_annotation(out);
    UpdateManyResult::render_lua_annotation(out);
    DeleteManyResult::render_lua_annotation(out);
    CreateManyResult::render_lua_annotation(out);
    VersionSummary::render_lua_annotation(out);
    ListVersionsResult::render_lua_annotation(out);
    render_crap_collections_create_many_lua(out);
    render_crap_collections_update_many_lua(out);
    render_crap_collections_delete_many_lua(out);
    ListVersionsOptions::render_lua_annotation(out);
    render_crap_collections_list_versions_lua(out);
    RestoreVersionOptions::render_lua_annotation(out);
    render_crap_collections_restore_version_lua(out);
    render_crap_collections_ref_count_lua(out);
}
fn render_crap_globals(out: &mut String) {
    // Only init-variant renders emit (init and pool produce the same
    // Lua function blocks — path-driven, not state-driven).
    render_crap_globals_init_root_lua(out);
    render_crap_globals_init_config_lua(out);
    GlobalGetOptions::render_lua_annotation(out);
    GlobalUpdateOptions::render_lua_annotation(out);
    GlobalUnpublishOptions::render_lua_annotation(out);
    GlobalValidateOptions::render_lua_annotation(out);
    render_crap_globals_get_lua(out);
    render_crap_globals_update_lua(out);
    render_crap_globals_unpublish_lua(out);
    render_crap_globals_validate_lua(out);
}
fn render_crap_hooks(out: &mut String) {
    render_crap_hooks_lua(out);
    HookEvent::render_lua_alias(out);
    out.push('\n');
}
fn render_crap_richtext(out: &mut String) {
    // Only the init variant + the stateless render emit their docs;
    // the pool variant produces identical `register_node` block.
    render_crap_richtext_init_lua(out);
    RichtextNodeSpec::render_lua_annotation(out);
    render_crap_richtext_render_lua(out);
    out.push('\n');
}
fn render_crap_log(out: &mut String) {
    render_crap_log_lua(out);
    out.push('\n');
}
fn render_crap_json(out: &mut String) {
    render_crap_json_lua(out);
    out.push('\n');
}
fn render_crap_util(out: &mut String) {
    render_crap_util_lua(out);
    out.push('\n');
}
fn render_crap_util_table_helpers(out: &mut String) {
    super::render_sentinel_blocks(include_str!("../../hooks/lua_api/util_helpers.lua"), out);
}
fn render_crap_util_string_helpers(_out: &mut String) {
    // Both table and string helpers come out of the same
    // `util_helpers.lua` file via `render_sentinel_blocks` — they're
    // emitted together by `render_crap_util_table_helpers`. This slot
    // is now a no-op; kept to preserve section count stability.
}
fn render_crap_auth(out: &mut String) {
    render_crap_auth_lua(out);
    out.push('\n');
}
fn render_crap_access(out: &mut String) {
    render_crap_access_init_lua(out);
    out.push('\n');
}
fn render_crap_env(out: &mut String) {
    render_crap_env_lua(out);
    out.push('\n');
}
fn render_crap_http(out: &mut String) {
    render_crap_http_lua(out);
    HttpRequest::render_lua_annotation(out);
    HttpResponse::render_lua_annotation(out);
    out.push('\n');
}
fn render_crap_email(out: &mut String) {
    render_crap_email_lua(out);
    EmailOptions::render_lua_annotation(out);
    out.push('\n');
}
fn render_crap_storage(out: &mut String) {
    render_crap_storage_lua(out);
    out.push('\n');
}
fn render_crap_config(out: &mut String) {
    render_crap_config_lua(out);
    out.push('\n');
}
fn render_crap_locale(out: &mut String) {
    render_crap_locale_lua(out);
    out.push('\n');
}
fn render_crap_crypto(out: &mut String) {
    // Stateless block first (6 fns; carries the section header), then
    // stateful (encrypt/decrypt). Both render fns emit at the same
    // `crap.crypto` path.
    render_crap_crypto_stateless_lua(out);
    render_crap_crypto_stateful_lua(out);
    out.push('\n');
}
fn render_crap_schema(out: &mut String) {
    render_crap_schema_init_lua(out);
    SchemaCollection::render_lua_annotation(out);
    SchemaField::render_lua_annotation(out);
    out.push('\n');
}
fn render_crap_jobs(out: &mut String) {
    // Only the init variant's render emits the header + first fn;
    // both init/pool produce identical `function crap.jobs.define(...)
    // end` blocks since the macro's render is path-driven.
    render_crap_jobs_init_lua(out);
    JobLabels::render_lua_annotation(out);
    JobDefinitionConfig::render_lua_annotation(out);
    JobHandlerContext::render_lua_annotation(out);
    JobInfo::render_lua_annotation(out);
    render_crap_jobs_queue_lua(out);
}
fn render_crap_pages(out: &mut String) {
    render_crap_pages_lua(out);
    PageOptions::render_lua_annotation(out);
    out.push_str(
        "\n--- @class crap.PageInfo\n\
         --- @field slug string The page slug (URL segment + template filename).\n\
         --- @field section? string Sidebar section heading.\n\
         --- @field label? string Sidebar label.\n\
         --- @field icon? string Material Symbols icon name.\n\
         --- @field access \"public\" | \"gated\"\n",
    );
    out.push('\n');
}
fn render_crap_routes(out: &mut String) {
    render_crap_routes_lua(out);
    RouteContext::render_lua_annotation(out);
    out.push_str(
        "\n--- A custom-route handler: receives the request context and returns a\n\
         --- response envelope `{ status?, headers?, cookies?, body?, json?, redirect? }`,\n\
         --- a bare string (200 text), or nil (404).\n\
         --- @alias crap.route_handler_fn fun(ctx: crap.RouteContext): table | string | nil\n\
         \n\
         --- @class crap.RouteInfo\n\
         --- @field path string The mounted path.\n\
         --- @field methods string[] HTTP methods this route answers.\n\
         --- @field access \"public\" | \"disabled\" | \"gated\"\n",
    );
    out.push('\n');
}
fn render_crap_template_data(out: &mut String) {
    render_crap_template_data_lua(out);
    out.push('\n');
}
fn render_crap_transaction(out: &mut String) {
    render_crap_transaction_lua(out);
    out.push('\n');
}
fn render_crap_tx(out: &mut String) {
    render_crap_tx_lua(out);
    out.push('\n');
}
fn render_crap_uploads(out: &mut String) {
    render_crap_uploads_lua(out);
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembled_output_matches_on_disk() {
        // The block-renderer assembly must produce a file byte-identical
        // to `types/crap.lua` at all times. Every later-phase migration
        // (each PR) re-runs this test plus `cargo xtask gen-lua-types
        // --check` to prove the diff is zero.
        let rendered = render_static_file();
        let on_disk = include_str!("../../../types/crap.lua");
        assert_eq!(
            rendered, on_disk,
            "assembled output diverged from on-disk types/crap.lua"
        );
    }

    #[test]
    fn block_render_count_matches_section_count() {
        // Sanity check: 33 section renderers. If a future PR adds a
        // namespace but forgets to add a renderer, this catches it
        // before the diff test does.
        assert_eq!(BLOCK_RENDERS.len(), 33);
    }

    #[test]
    fn render_static_file_is_idempotent() {
        // Two back-to-back renders must produce byte-identical output.
        // Guards against any future hashmap/hashset iteration creeping
        // into the derive pipeline (e.g. LuaAlias variant order,
        // LuaAnnotation field order, lua_table fn order). The
        // assembled_output diff test already catches this implicitly
        // by comparing to the on-disk file, but an explicit
        // idempotency check makes the determinism contract a
        // first-class invariant.
        let a = render_static_file();
        let b = render_static_file();
        assert_eq!(
            a, b,
            "render_static_file produced different output on two consecutive runs — non-deterministic ordering in a derive"
        );
    }
}
