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
    FieldHooks, FieldTab, FieldType, Hooks, IndexDefinition, Labels, LocalizedString,
    McpFieldConfig, RelationshipConfig, SelectOption, VersionsConfig,
    collection::{
        Access, Activation, AdminConfig, Auth, AuthMethod, CollectionDefinition, GlobalDefinition,
        McpConfig, Surface,
    },
    field::{FieldHookFn, FieldWidth, JoinConfig, PickerAppearance, ValidateFunction},
    job::JobLabels,
    upload::{CollectionUpload, FormatOptions, FormatQuality, ImageFit, ImageSize},
};
use crate::db::query::PaginationResult;
use crate::hooks::lifecycle::{
    AccessContext, AuthStrategyContext, FieldHookContext, HookContext, HookEvent,
    JobHandlerContext, JobInfo, ValidateContext,
};
use crate::hooks::lua_api::{
    access::render_crap_access_init_lua,
    auth::render_crap_auth_lua,
    collections::{render_crap_collections_init_config_lua, render_crap_collections_init_root_lua},
    config::{render_crap_config_lua, render_crap_locale_lua},
    crud::{
        collection::{
            bulk::{
                create_many::render_crap_collections_create_many_lua,
                delete_many::{
                    DeleteManyQueryInput, DeleteManyResult, render_crap_collections_delete_many_lua,
                },
                update_many::{UpdateManyQueryInput, render_crap_collections_update_many_lua},
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
            update::{GlobalUpdateOptions, render_crap_globals_update_lua},
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
    schema::{SchemaCollection, SchemaField, render_crap_schema_init_lua},
    template_data::render_crap_template_data_lua,
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
    render_crap_config,
    render_crap_locale,
    render_crap_crypto,
    render_crap_schema,
    render_crap_jobs,
    render_crap_pages,
    render_crap_template_data,
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
    Hooks::render_lua_annotation(out);
    Access::render_lua_annotation(out);
    AuthStrategyContext::render_lua_annotation(out);
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
    AccessContext::render_lua_annotation(out);
}
fn render_crap_collections(out: &mut String) {
    render_crap_collections_init_root_lua(out);
    render_crap_collections_init_config_lua(out);
    // `find` and `find_by_id` are intentionally NOT emitted into the
    // static crap.lua: their typed signatures live exclusively in the
    // per-project `generated.lua` so the per-collection
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
    render_crap_globals_get_lua(out);
    render_crap_globals_update_lua(out);
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
    out.push('\n');
}
fn render_crap_template_data(out: &mut String) {
    render_crap_template_data_lua(out);
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
        // Sanity check: 28 section renderers. If a future PR adds a
        // namespace but forgets to add a renderer, this catches it
        // before the diff test does.
        assert_eq!(BLOCK_RENDERS.len(), 28);
    }
}
