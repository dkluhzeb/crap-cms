//! Running every boot gate over the loaded definitions, with every problem
//! reported together rather than the first — an upgrade that trips several
//! independent ones costs one fix-and-restart cycle, not one each.

use anyhow::{Context as _, Result};
use mlua::Lua;

use crate::{
    config::{CrapConfig, ErrorReport},
    core::Registry,
    hooks::lifecycle::reset_instruction_budget,
};

use super::{
    validate_admin_access_ref, validate_admin_default_sorts, validate_auth_methods,
    validate_hook_references, validate_job_schedules, validate_join_limits,
    validate_locale_field_collisions, validate_pages, validate_relation_targets,
    validate_required_locales, validate_richtext_nodes, validate_routes, validate_row_conditions,
    validate_table_name_collisions, warn_mcp_reserved_field_shadowing, warn_public_lifecycle_views,
};

/// The checks over the loaded definitions, every one of them run and every
/// problem reported together. Statically-known refs and settings fail the
/// boot here instead of surfacing at first request.
pub(crate) fn run_startup_checks(
    lua: &Lua,
    snapshot: &Registry,
    config: &CrapConfig,
) -> Result<()> {
    let mut report = ErrorReport::new();

    // Resolving hook refs runs the required modules' top level, under a fresh
    // budget.
    reset_instruction_budget(lua);
    report.check(
        validate_hook_references(lua, snapshot).context("Hook/access reference validation failed"),
    );

    check_routes_and_gates(lua, config, &mut report);
    check_definitions(snapshot, config, &mut report);

    // Advisory warning (not a hard error): with default_deny = false, a
    // collection's draft/trash view with no gating rule is world-readable.
    warn_public_lifecycle_views(snapshot, config.access.default_deny);

    // Advisory warning: a field whose name is a reserved MCP tool argument is
    // shadowed on that surface (its value is dropped there).
    warn_mcp_reserved_field_shadowing(snapshot, config.mcp.enabled);

    report.into_result()
}

/// Custom pages and routes must resolve and not collide — fail to boot rather
/// than 500 (or panic at router assembly) on first request; the `[admin]`
/// access gate ref must resolve — the runtime gate fails closed, so a typo
/// would lock everyone out of the admin panel.
fn check_routes_and_gates(lua: &Lua, config: &CrapConfig, report: &mut ErrorReport) {
    report.check(validate_pages(lua).context("Custom page validation failed"));
    report.check(
        validate_routes(lua, &config.routes.prefix).context("Custom route validation failed"),
    );
    report.check(
        validate_admin_access_ref(lua, config.admin.access.as_ref())
            .context("Admin access gate validation failed"),
    );
}

/// The checks over the definitions themselves.
///
/// - field names colliding with the generated `{name}__{locale}` columns;
/// - `required_locales` naming unconfigured locales (a typo would fail every
///   non-draft write with a confusing `validation.required_locale`);
/// - relationship / upload / join targets that are not registered (they
///   would fail the ref-count recompute and every reference write);
/// - joins listing more than `[pagination] max_limit` documents;
/// - rich text fields naming unregistered custom nodes (the node would be
///   lost in the editor, validation and search);
/// - generated table names that collide (`posts_tags` vs the `tags` array
///   of `posts`);
/// - structurally invalid `auth.methods` (warnings for footguns);
/// - cron `schedule`s the scheduler can't parse (indistinguishable from
///   "not due yet" otherwise);
/// - `admin.default_sort` naming a column the table does not have.
fn check_definitions(snapshot: &Registry, config: &CrapConfig, report: &mut ErrorReport) {
    let locales = &config.locale.locales;

    report.check(
        validate_locale_field_collisions(snapshot, locales)
            .context("Locale/field-name collision detected"),
    );
    report.check(
        validate_required_locales(snapshot, locales)
            .context("Invalid required_locales configuration"),
    );
    report.check(
        validate_relation_targets(snapshot).context("Relationship target validation failed"),
    );
    report.check(
        validate_join_limits(snapshot, config.pagination.max_limit)
            .context("Join limit validation failed"),
    );
    report.check(validate_richtext_nodes(snapshot).context("Rich text node validation failed"));
    report.check(validate_row_conditions(snapshot).context("Display condition validation failed"));
    report.check(validate_table_name_collisions(snapshot).context("Table name collision detected"));
    report.check(validate_auth_methods(snapshot).context("Auth method configuration invalid"));
    report.check(validate_job_schedules(snapshot).context("Job schedule validation failed"));
    report.check(
        validate_admin_default_sorts(snapshot).context("admin.default_sort validation failed"),
    );
}
