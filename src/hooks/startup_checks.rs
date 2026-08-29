//! Post-registry validation of hook and access references (BUG-5).
//!
//! Operators write hook/access refs as plain strings in collection, global,
//! and field definitions. Without this pass, a typo like
//! `"hooks.field_hooks.slugifyy"` only surfaces at the first request that
//! triggers the hook. This module walks the registry at startup, attempts
//! to resolve every statically-known ref against the init-time Lua VM, and
//! returns a single aggregated error listing every unresolved ref with its
//! source location.
//!
//! Scope (intentionally reduced): collection + global + field hook and
//! access refs. Job handlers and dynamic hook registrations (via
//! `crap.hooks.register`) are NOT validated here — they may resolve through
//! different mechanisms at runtime.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use anyhow::{Result, bail};
use mlua::{Lua, LuaSerdeExt as _, Table, Value};
use tracing::warn;

use crate::admin::custom_routes::is_reserved_path;
use crate::core::{
    Access, FieldDefinition, FieldType, HookRef, Hooks, Registry, RequiredLocales, SchemaStep,
    Slug,
    collection::{Activation, AuthMethod, MfaMode, Surface},
    walk_all_fields,
};
use crate::db::query::helpers::{global_table, join_table, prefixed_name, walk_leaf_fields};
use crate::hooks::lifecycle::resolve_hook_function;
use crate::hooks::lua_api::routes::ROUTES_KEY;

/// Validate every statically-known hook and access reference in the registry.
///
/// Returns `Ok(())` when every ref resolves cleanly. Returns `Err` with a
/// single aggregated message listing every unresolved ref and its source.
///
/// Must be called after `init_lua` so `require(...)` can locate modules
/// under `{config_dir}/hooks/` (path configured by `setup_package_paths`).
pub fn validate_hook_references(lua: &Lua, registry: &Registry) -> Result<()> {
    let mut missing: Vec<String> = Vec::new();

    for (slug, def) in &registry.collections {
        check_hooks(
            lua,
            &def.hooks,
            &format!("collection '{slug}'"),
            &mut missing,
        );
        check_access(
            lua,
            &def.access,
            &format!("collection '{slug}'"),
            &mut missing,
        );
        check_field_list(
            lua,
            &def.fields,
            &format!("collection '{slug}'"),
            &mut missing,
        );
    }

    for (slug, def) in &registry.globals {
        check_hooks(lua, &def.hooks, &format!("global '{slug}'"), &mut missing);
        check_access(lua, &def.access, &format!("global '{slug}'"), &mut missing);
        check_field_list(lua, &def.fields, &format!("global '{slug}'"), &mut missing);
    }

    if missing.is_empty() {
        return Ok(());
    }

    let body = missing.join("\n  - ");
    bail!(
        "Unresolved hook/access references at startup:\n  - {body}\n\n\
         Each line shows `source: kind: 'ref'`. Either create the Lua module/function, \
         fix the typo, or remove the reference from the definition."
    );
}

/// Validate custom routes registered via `crap.routes.register`: every handler
/// and gated-access ref must resolve, no two routes may claim the same
/// (method, path) — a collision would panic at router assembly — and the
/// **mounted** path (`prefix` + `path`) must not fall under a reserved built-in
/// prefix (a reserved `prefix` would otherwise crash router assembly even though
/// each individual `path` passed the per-route check at registration). Runs on
/// the init VM after `init.lua`.
///
/// Returns `Ok(())` when the registry is clean (or empty). Returns `Err` with an
/// aggregated message otherwise.
pub fn validate_routes(lua: &Lua, prefix: &str) -> Result<()> {
    let Ok(routes): mlua::Result<Table> = lua.named_registry_value(ROUTES_KEY) else {
        return Ok(());
    };

    let prefix = prefix.trim_end_matches('/');
    let mut errors: Vec<String> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    // A non-empty prefix must be a clean absolute path, else `Router::route`
    // panics at assembly (e.g. `routes.prefix = "api"` → `"api/x"`).
    if !prefix.is_empty()
        && (!prefix.starts_with('/') || prefix.contains("..") || prefix.contains("//"))
    {
        errors.push(format!(
            "routes.prefix {prefix:?} must be an absolute path with no '..' or '//' segments"
        ));
    }

    for entry in routes.sequence_values::<Table>() {
        let Ok(entry) = entry else { continue };
        let path: String = entry.get("path").unwrap_or_default();
        let methods = entry
            .get::<Table>("methods")
            .map(|t| t.sequence_values::<String>().flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        let label = format!("{} {path}", methods.join(","));

        // The mounted path (prefix + path) must not land under a reserved
        // built-in prefix — a reserved `prefix` slips past the per-route check
        // and would panic at router assembly.
        if !prefix.is_empty() && is_reserved_path(&format!("{prefix}{path}")) {
            errors.push(format!(
                "route '{label}': mounted path '{prefix}{path}' collides with a reserved prefix \
                 (change routes.prefix or the route path)"
            ));
        }

        // A GET route also answers HEAD (mount ORs it in), so GET occupies the
        // HEAD slot too. Account for that here so a separate HEAD registration
        // on the same path is reported as a duplicate instead of panicking at
        // router assembly when the two MethodRouters merge.
        let mut occupied: Vec<String> = methods.clone();
        if methods.iter().any(|m| m == "GET") && !methods.iter().any(|m| m == "HEAD") {
            occupied.push("HEAD".to_string());
        }
        for m in &occupied {
            if !seen.insert((m.clone(), path.clone())) {
                errors.push(format!(
                    "duplicate route: {m} {path} (note: a GET route also answers HEAD)"
                ));
            }
        }

        check_route_ref(lua, &entry, "handler", &label, &mut errors);

        // `access` is a ref only when gated (not nil / true / false).
        if let Ok(v @ (Value::String(_) | Value::Table(_))) = entry.get::<Value>("access") {
            check_resolvable(lua, v, "access", &label, &mut errors);
        }
    }

    if errors.is_empty() {
        return Ok(());
    }

    let body = errors.join("\n  - ");
    bail!(
        "Custom route problems at startup:\n  - {body}\n\n\
         Create the Lua handler/access module, fix the typo, or remove the route."
    );
}

/// Resolve a required ref field (`handler`) on a route entry.
fn check_route_ref(lua: &Lua, entry: &Table, field: &str, label: &str, out: &mut Vec<String>) {
    match entry.get::<Value>(field) {
        Ok(v) => check_resolvable(lua, v, field, label, out),
        Err(_) => out.push(format!("route '{label}': missing {field}")),
    }
}

/// Decode a value as a `HookRef` and confirm it resolves to a Lua function.
fn check_resolvable(lua: &Lua, value: Value, field: &str, label: &str, out: &mut Vec<String>) {
    match lua.from_value::<HookRef>(value) {
        Ok(hook_ref) => {
            if resolve_hook_function(lua, hook_ref.reference()).is_err() {
                out.push(format!(
                    "route '{label}': {field} '{}'",
                    hook_ref.reference()
                ));
            }
        }
        Err(e) => out.push(format!("route '{label}': invalid {field} ref: {e}")),
    }
}

/// Collect any unresolved refs in a `Hooks` struct.
fn check_hooks(lua: &Lua, hooks: &Hooks, source: &str, out: &mut Vec<String>) {
    let pairs: [(&str, &[HookRef]); 8] = [
        ("before_validate", &hooks.before_validate),
        ("before_change", &hooks.before_change),
        ("after_change", &hooks.after_change),
        ("before_read", &hooks.before_read),
        ("after_read", &hooks.after_read),
        ("before_delete", &hooks.before_delete),
        ("after_delete", &hooks.after_delete),
        ("before_broadcast", &hooks.before_broadcast),
    ];

    for (kind, refs) in pairs {
        for r in refs {
            if resolve_hook_function(lua, r.reference()).is_err() {
                out.push(format!("{source}: {kind}: '{}'", r.reference()));
            }
        }
    }
}

/// Validate per-collection `auth.methods` configurations.
///
/// Hard errors (boot fails):
/// - `enabled = true` with no methods listed.
/// - Duplicate `password_login` or `bearer` on one collection.
/// - Any method with an empty `surfaces` set — would silently
///   never fire on any request, almost certainly a config mistake.
/// - Strategy with `activates_on = { header = "" }` — would
///   silently never match (no HTTP header has an empty name).
/// - Strategy with empty `authenticate` — no Lua hook to invoke.
///
/// Soft warnings (logged, boot continues):
/// - `Always`-activated strategies (potential footgun — fires on
///   every request that reaches the surface).
/// - Multiple `Always` strategies sharing a surface (request-
///   authentication outcome depends on registration order).
pub fn validate_auth_methods(registry: &Registry) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();
    let mut always_by_surface: HashMap<Surface, Vec<(String, String)>> = HashMap::new();

    for (slug, def) in &registry.collections {
        let Some(auth) = def.auth.as_ref() else {
            continue;
        };
        if !auth.enabled {
            continue;
        }
        if auth.methods.is_empty() {
            errors.push(format!(
                "collection '{slug}': auth.enabled is true but auth.methods is empty. \
                 Use crap.auth.default_methods() for the standard set."
            ));
            continue;
        }
        check_one_collection_methods(slug, &auth.methods, &mut errors, &mut always_by_surface);
    }

    warn_on_always_cross_collection_collisions(&always_by_surface);

    if errors.is_empty() {
        return Ok(());
    }
    bail!(
        "Auth method configuration errors:\n  - {}",
        errors.join("\n  - ")
    );
}

/// Walk a single collection's methods, collecting per-method shape
/// errors and tracking `Always`-active strategies for the cross-
/// collection collision warning emitted by the caller.
fn check_one_collection_methods(
    slug: &Slug,
    methods: &[AuthMethod],
    errors: &mut Vec<String>,
    always_by_surface: &mut HashMap<Surface, Vec<(String, String)>>,
) {
    let mut password_count = 0;
    let mut bearer_count = 0;
    for m in methods {
        if let Some(s) = method_surfaces(m)
            && s.is_empty()
        {
            errors.push(format!(
                "collection '{slug}': method has empty `surfaces` list — \
                 it can never fire. Drop the method or list at least one surface."
            ));
        }
        match m {
            AuthMethod::PasswordLogin {
                mfa, mfa_deliver, ..
            } => {
                password_count += 1;

                // `custom` MFA and its delivery hook come as a PAIR — a custom
                // mode with no hook strands every login (no code delivered), a
                // hook without the mode is silently dead config.
                if *mfa == MfaMode::Custom && mfa_deliver.is_none() {
                    errors.push(format!(
                        "Collection '{slug}': mfa = \"custom\" requires an mfa_deliver hook"
                    ));
                }
                if *mfa != MfaMode::Custom && mfa_deliver.is_some() {
                    errors.push(format!(
                        "Collection '{slug}': mfa_deliver is only valid with mfa = \"custom\" (got mfa = \"{}\")",
                        match mfa {
                            MfaMode::Email => "email",
                            MfaMode::Off => "false",
                            MfaMode::Custom => unreachable!(),
                        }
                    ));
                }
            }
            AuthMethod::Bearer { .. } => bearer_count += 1,
            AuthMethod::Strategy {
                name,
                authenticate,
                activates_on,
                surfaces,
            } => check_strategy_shape(
                slug,
                name,
                authenticate.reference(),
                activates_on,
                surfaces,
                errors,
                always_by_surface,
            ),
            AuthMethod::SessionCookie { .. } => {}
        }
    }
    if password_count > 1 {
        errors.push(format!(
            "collection '{slug}': multiple password_login methods declared (one is enough)."
        ));
    }
    if bearer_count > 1 {
        errors.push(format!(
            "collection '{slug}': multiple bearer methods declared (one is enough)."
        ));
    }
}

fn method_surfaces(m: &AuthMethod) -> Option<&crate::core::collection::SurfaceSet> {
    match m {
        AuthMethod::PasswordLogin { .. } => None,
        AuthMethod::Bearer { surfaces }
        | AuthMethod::SessionCookie { surfaces }
        | AuthMethod::Strategy { surfaces, .. } => Some(surfaces),
    }
}

fn check_strategy_shape(
    slug: &Slug,
    name: &str,
    authenticate: &str,
    activates_on: &Activation,
    surfaces: &crate::core::collection::SurfaceSet,
    errors: &mut Vec<String>,
    always_by_surface: &mut HashMap<Surface, Vec<(String, String)>>,
) {
    if authenticate.trim().is_empty() {
        errors.push(format!(
            "collection '{slug}': strategy '{name}' has empty `authenticate` \
             — no Lua hook to invoke."
        ));
    }
    if let Activation::Header { header } = activates_on
        && header.trim().is_empty()
    {
        errors.push(format!(
            "collection '{slug}': strategy '{name}' has empty \
             `activates_on.header` — no HTTP header has an empty \
             name, so the strategy could never fire."
        ));
    }
    if matches!(activates_on, Activation::Always { .. }) {
        warn!(
            "collection '{slug}': strategy '{name}' is always-active on every request. \
             Consider a header discriminator for safer scoping."
        );
        for surface in surfaces {
            always_by_surface
                .entry(*surface)
                .or_default()
                .push((slug.to_string(), name.to_string()));
        }
    }
}

fn warn_on_always_cross_collection_collisions(
    always_by_surface: &HashMap<Surface, Vec<(String, String)>>,
) {
    for (surface, owners) in always_by_surface {
        if owners.len() > 1 {
            let list = owners
                .iter()
                .map(|(s, n)| format!("'{s}'.'{n}'"))
                .collect::<Vec<_>>()
                .join(", ");
            warn!(
                "multiple always-active strategies on surface {surface:?}: {list}. \
                 Request authentication may depend on registration order — prefer \
                 header discriminators."
            );
        }
    }
}

/// Warn when `default_deny = false` leaves a draft or trash view ungated. With
/// `default_deny = false`, a view whose access resolves to `None` (no own key,
/// and no `update` fallback) is allowed for *everyone* — so a collection with
/// `drafts`/`soft_delete` enabled but no `draft`/`trash`/`update` rule exposes
/// its unpublished and soft-deleted rows to unauthenticated callers. Globals
/// have no soft-delete, but the same footgun applies to their `draft` view.
/// `read` (published) is intentionally not flagged: published content being
/// public is the expected default; the footgun is drafts/trash silently joining
/// it. Advisory only (the config is valid, just dangerous).
pub fn warn_public_lifecycle_views(registry: &Registry, default_deny: bool) {
    if default_deny {
        return;
    }

    for (slug, def) in &registry.collections {
        for view in def.publicly_exposed_lifecycle_views(default_deny) {
            let documents = match view {
                "trash" => "soft-deleted",
                _ => view,
            };

            warn!(
                "Collection '{slug}': access.default_deny is false and the '{view}' view has \
                 no access.{view} (or access.update) rule — {documents} documents are visible \
                 to EVERYONE, including unauthenticated callers. Add an access.{view} (or \
                 access.update) rule, or set access.default_deny = true."
            );
        }
    }

    for (slug, def) in &registry.globals {
        if def.draft_view_publicly_exposed(default_deny) {
            warn!(
                "Global '{slug}': access.default_deny is false and the 'draft' view has \
                 no access.draft (or access.update) rule — draft documents are visible \
                 to EVERYONE, including unauthenticated callers. Add an access.draft (or \
                 access.update) rule, or set access.default_deny = true."
            );
        }
    }
}

/// Reject field names that collide with the generated locale-suffixed column
/// pattern `{field}__{locale}`. If a user defines a literal field named
/// `title__en` while `en` is a configured locale, the generated localized
/// column for `title` would be `title__en` — a silent collision. Fail startup.
pub fn validate_locale_field_collisions(registry: &Registry, locales: &[String]) -> Result<()> {
    if locales.is_empty() {
        return Ok(());
    }

    let mut collisions: Vec<String> = Vec::new();

    for (slug, def) in &registry.collections {
        walk_fields_for_collisions(
            &def.fields,
            locales,
            &format!("collection '{slug}'"),
            &mut collisions,
        );
    }

    for (slug, def) in &registry.globals {
        walk_fields_for_collisions(
            &def.fields,
            locales,
            &format!("global '{slug}'"),
            &mut collisions,
        );
    }

    if collisions.is_empty() {
        return Ok(());
    }

    let body = collisions.join("\n  - ");
    bail!(
        "Field name collides with locale-suffixed column pattern '{{name}}__{{locale}}':\n  - {body}\n\n\
         Rename the field (or change its locale suffix) to avoid a silent collision \
         with the generated localized column."
    );
}

fn walk_fields_for_collisions(
    fields: &[FieldDefinition],
    locales: &[String],
    source: &str,
    out: &mut Vec<String>,
) {
    walk_all_fields(fields, &mut Vec::new(), &mut |f, _| {
        for loc in locales {
            let suffix = format!("__{loc}");
            if f.name.ends_with(&suffix) && f.name.len() > suffix.len() {
                out.push(format!(
                    "{source} field '{}': ends with locale suffix '{}'",
                    f.name, suffix
                ));
            }
        }
    });
}

/// Validate every `required_locales` setting against the configured locales, so
/// a typo (`"de-DE"` when only `"de"` is configured) fails to boot instead of
/// silently making every non-draft write to that field error at runtime.
///
/// Checks the collection-level default and every field-level override (recursing
/// through groups, blocks, tabs). `All` is always valid (it expands to the
/// configured set). A `List` code must be a configured locale; setting any
/// `required_locales` while localization is disabled is also rejected.
///
/// Returns `Err` with a single aggregated message listing every offending
/// setting and its source.
pub fn validate_required_locales(registry: &Registry, locales: &[String]) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    for (slug, def) in &registry.collections {
        check_required_locales_value(
            def.required_locales.as_ref(),
            locales,
            &format!("collection '{slug}' (collection-level required_locales)"),
            &mut errors,
        );
        walk_fields_for_required_locales(
            &def.fields,
            locales,
            &format!("collection '{slug}'"),
            &mut errors,
        );
    }

    // Globals have no collection-level `required_locales`; only field overrides.
    for (slug, def) in &registry.globals {
        walk_fields_for_required_locales(
            &def.fields,
            locales,
            &format!("global '{slug}'"),
            &mut errors,
        );
    }

    if errors.is_empty() {
        return Ok(());
    }

    let body = errors.join("\n  - ");
    bail!(
        "Invalid `required_locales` configuration:\n  - {body}\n\n\
         `required_locales` may only be set on locale-scoped fields, and every \
         listed locale must appear in `[locale].locales`."
    );
}

/// Validate a single `required_locales` value against the configured locales.
fn check_required_locales_value(
    rl: Option<&RequiredLocales>,
    locales: &[String],
    source: &str,
    out: &mut Vec<String>,
) {
    let Some(rl) = rl else {
        return;
    };

    if locales.is_empty() {
        out.push(format!(
            "{source} sets required_locales but localization is not enabled \
             (no `[locale].locales` configured)"
        ));
        return;
    }

    if let RequiredLocales::List(list) = rl {
        for code in list {
            if !locales.iter().any(|l| l == code) {
                out.push(format!(
                    "{source}: unknown locale '{code}' (configured: {})",
                    locales.join(", ")
                ));
            }
        }
    }
}

/// Walk a field tree validating every `required_locales` override.
///
/// Fields nested inside Array/Blocks rows are skipped: completeness treats
/// those as opaque leaf join targets, so a `required_locales` nested inside
/// one is inert and not validated here. Localization scope matches the
/// completeness check (and the migration DDL): only enclosing *groups* carry
/// `localized` down — layout wrappers don't.
fn walk_fields_for_required_locales(
    fields: &[FieldDefinition],
    locales: &[String],
    source: &str,
    out: &mut Vec<String>,
) {
    walk_all_fields(fields, &mut Vec::new(), &mut |f, path| {
        let inside_join_subtree = path
            .iter()
            .any(|s| matches!(s, SchemaStep::Field(p) if p.field_type.has_rows()));
        if inside_join_subtree {
            return;
        }

        let Some(rl) = f.required_locales.as_ref() else {
            return;
        };

        let ctx = format!("{source} field '{}'", f.name);

        // `required_locales` is only meaningful on a locale-scoped field —
        // one that is `localized`, or inherits localization from an
        // enclosing group. This honors inheritance (the per-field parser
        // can't), matching how the completeness check resolves scope.
        let inherited_localized = path.iter().any(|s| {
            matches!(s, SchemaStep::Field(p)
                if p.field_type == FieldType::Group && p.localized)
        });
        if !f.is_locale_scoped(inherited_localized) {
            out.push(format!(
                "{ctx}: required_locales only applies to localized fields \
                 (mark the field — or its enclosing group — localized)"
            ));
        }

        check_required_locales_value(Some(rl), locales, &ctx, out);
    });
}

/// Reject definitions whose generated DB table names collide.
///
/// Every collection produces a main table named after its slug; every global a
/// `_global_{slug}` table; and array / blocks / has-many-relationship fields
/// produce join tables named `{slug}_{field}` (with `{group}__` prefixes for
/// nested groups). Because slugs may themselves contain `_`, a collection
/// slugged `posts_tags` collides with the `tags` array field of a `posts`
/// collection — and the migration layer would then silently ALTER one table as
/// if it were the other. Reject the collision up front instead.
pub fn validate_table_name_collisions(registry: &Registry) -> Result<()> {
    let mut owners: HashMap<String, String> = HashMap::new();
    let mut conflicts: Vec<String> = Vec::new();

    for (slug, def) in &registry.collections {
        claim_table(
            &mut owners,
            &mut conflicts,
            slug.to_string(),
            format!("collection '{slug}'"),
        );
        collect_field_tables(slug, &def.fields, &mut owners, &mut conflicts);
    }

    for slug in registry.globals.keys() {
        claim_table(
            &mut owners,
            &mut conflicts,
            global_table(slug),
            format!("global '{slug}'"),
        );
    }

    if conflicts.is_empty() {
        return Ok(());
    }

    let body = conflicts.join("\n  - ");
    bail!(
        "Generated table name collision(s):\n  - {body}\n\n\
         Rename the offending collection slug or field so each generated table \
         name is unique."
    );
}

/// Record `table` as owned by `owner`; if a different owner already claimed it,
/// push a conflict message instead.
fn claim_table(
    owners: &mut HashMap<String, String>,
    conflicts: &mut Vec<String>,
    table: String,
    owner: String,
) {
    match owners.get(&table) {
        Some(existing) if existing != &owner => {
            conflicts.push(format!(
                "'{table}' is generated by both {existing} and {owner}"
            ));
        }
        Some(_) => {}
        None => {
            owners.insert(table, owner);
        }
    }
}

/// Walk a field tree collecting the join-table names it generates, mirroring
/// the migration orchestrator's traversal (`sync_join_tables_inner`). The
/// flat-column walk ([`walk_leaf_fields`]) handles Group `__`-prefixing and
/// transparent layout wrappers; Array/Blocks/has-many fields are the leaf
/// columns that own join tables.
fn collect_field_tables(
    slug: &str,
    fields: &[FieldDefinition],
    owners: &mut HashMap<String, String>,
    conflicts: &mut Vec<String>,
) {
    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        let full_name = prefixed_name(prefix, &field.name);

        let is_has_many_ref = field.field_type.is_reference()
            && field.relationship.as_ref().is_some_and(|rc| rc.has_many);

        if is_has_many_ref {
            claim_table(
                owners,
                conflicts,
                join_table(slug, &full_name),
                format!("has-many field '{full_name}' of collection '{slug}'"),
            );
        } else if field.field_type.has_rows() {
            claim_table(
                owners,
                conflicts,
                join_table(slug, &full_name),
                format!("field '{full_name}' of collection '{slug}'"),
            );
        }

        Ok(())
    });
}

/// Collect any unresolved refs in an `Access` struct.
fn check_access(lua: &Lua, access: &Access, source: &str, out: &mut Vec<String>) {
    let pairs: [(&str, Option<&str>); 10] = [
        ("access.read", access.read.as_ref().map(HookRef::reference)),
        (
            "access.create",
            access.create.as_ref().map(HookRef::reference),
        ),
        (
            "access.update",
            access.update.as_ref().map(HookRef::reference),
        ),
        (
            "access.delete",
            access.delete.as_ref().map(HookRef::reference),
        ),
        (
            "access.trash",
            access.trash.as_ref().map(HookRef::reference),
        ),
        (
            "access.draft",
            access.draft.as_ref().map(HookRef::reference),
        ),
        (
            "access.versions",
            access.versions.as_ref().map(HookRef::reference),
        ),
        (
            "access.unlock",
            access.unlock.as_ref().map(HookRef::reference),
        ),
        (
            "access.admin",
            access.admin.as_ref().map(HookRef::reference),
        ),
        ("access.mcp", access.mcp.as_ref().map(HookRef::reference)),
    ];

    for (kind, maybe_ref) in pairs {
        let Some(r) = maybe_ref else { continue };
        if resolve_hook_function(lua, r).is_err() {
            out.push(format!("{source}: {kind}: '{r}'"));
        }
    }
}

/// Render the `{source} field 'a' block 'b' …` source label for a field from
/// its [`SchemaStep`] ancestor chain.
fn field_source_label(source: &str, path: &[SchemaStep<'_>], field: &FieldDefinition) -> String {
    let mut label = String::from(source);

    for step in path {
        match step {
            SchemaStep::Field(f) => {
                let _ = write!(label, " field '{}'", f.name);
            }
            SchemaStep::Block(b) => {
                let _ = write!(label, " block '{}'", b.block_type);
            }
            SchemaStep::Tab { index, tab } => {
                let _ = write!(label, " tab #{} ('{}')", index, tab.label);
            }
        }
    }

    let _ = write!(label, " field '{}'", field.name);
    label
}

/// Walk a field list (including layout wrappers, groups, arrays, blocks, tabs)
/// and collect every unresolved field hook / access reference.
fn check_field_list(lua: &Lua, fields: &[FieldDefinition], source: &str, out: &mut Vec<String>) {
    walk_all_fields(fields, &mut Vec::new(), &mut |f, path| {
        let field_src = field_source_label(source, path, f);

        // field-level hooks
        let hook_pairs: [(&str, &[HookRef]); 4] = [
            ("before_validate", &f.hooks.before_validate),
            ("before_change", &f.hooks.before_change),
            ("after_change", &f.hooks.after_change),
            ("after_read", &f.hooks.after_read),
        ];
        for (kind, refs) in hook_pairs {
            for r in refs {
                if resolve_hook_function(lua, r.reference()).is_err() {
                    out.push(format!("{field_src}: {kind}: '{}'", r.reference()));
                }
            }
        }

        // field-level access
        let access_pairs: [(&str, Option<&str>); 3] = [
            (
                "access.read",
                f.access.read.as_ref().map(HookRef::reference),
            ),
            (
                "access.create",
                f.access.create.as_ref().map(HookRef::reference),
            ),
            (
                "access.update",
                f.access.update.as_ref().map(HookRef::reference),
            ),
        ];
        for (kind, maybe_ref) in access_pairs {
            let Some(r) = maybe_ref else { continue };
            if resolve_hook_function(lua, r).is_err() {
                out.push(format!("{field_src}: {kind}: '{r}'"));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use mlua::{Lua, LuaOptions, StdLib};

    use crate::core::{Access, CollectionDefinition, FieldDefinition, FieldType, Hooks, Registry};

    use super::*;

    fn sandboxed_lua() -> Lua {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).unwrap();
        crate::hooks::sandbox_lua(&lua).unwrap();
        lua
    }

    /// Missing ref surfaces at startup with collection + kind in the message.
    #[test]
    fn validate_hook_references_reports_missing_collection_hook() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("posts");
        def.hooks = Hooks::builder()
            .before_change(vec![HookRef::new("hooks.missing.module")])
            .build();

        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        let err = validate_hook_references(&lua, &registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("posts"), "expected slug in msg: {msg}");
        assert!(msg.contains("before_change"), "expected kind in msg: {msg}");
        assert!(
            msg.contains("hooks.missing.module"),
            "expected ref in msg: {msg}"
        );
    }

    /// Missing field-level access ref surfaces with the field name too.
    #[test]
    fn validate_hook_references_reports_missing_field_access() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("posts");
        let mut field = FieldDefinition::builder("title", FieldType::Text).build();
        field.access.read = Some(HookRef::new("hooks.never.exists"));
        def.fields = vec![field];

        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        let err = validate_hook_references(&lua, &registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("title"), "expected field name: {msg}");
        assert!(msg.contains("access.read"), "expected kind: {msg}");
        assert!(msg.contains("hooks.never.exists"), "expected ref: {msg}");
    }

    /// A clean registry (no hook refs) validates without error.
    #[test]
    fn validate_hook_references_passes_when_no_refs() {
        let lua = sandboxed_lua();
        let def = CollectionDefinition::new("posts");
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        validate_hook_references(&lua, &registry.read().unwrap()).expect("no refs means no errors");
    }

    /// A field literally named `{name}__{locale}` collides with the generated
    /// localized column — reject at startup.
    #[test]
    fn locale_config_rejects_field_name_collision() {
        let mut def = CollectionDefinition::new("posts");
        // `title__en` collides with the generated locale suffix for `en`.
        def.fields = vec![FieldDefinition::builder("title__en", FieldType::Text).build()];
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        let locales = vec!["en".to_string(), "de".to_string()];
        let err =
            validate_locale_field_collisions(&registry.read().unwrap(), &locales).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("title__en"), "expected field name: {msg}");
        assert!(msg.contains("__en"), "expected locale suffix: {msg}");
        assert!(msg.contains("posts"), "expected slug: {msg}");
    }

    /// Locale collisions are skipped entirely when no locales are configured.
    #[test]
    fn locale_field_collisions_noop_when_no_locales() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![FieldDefinition::builder("title__en", FieldType::Text).build()];
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        validate_locale_field_collisions(&registry.read().unwrap(), &[])
            .expect("no locales = no check");
    }

    /// Unrelated suffixes are fine.
    #[test]
    fn locale_field_collisions_allows_unrelated_names() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("title__fr", FieldType::Text).build(),
        ];
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        // `fr` is not in the list, so `title__fr` is just a literal name.
        let locales = vec!["en".to_string(), "de".to_string()];
        validate_locale_field_collisions(&registry.read().unwrap(), &locales)
            .expect("no collision when suffix does not match an enabled locale");
    }

    /// A collection slug that equals another collection's array join-table
    /// name (`posts` + array `tags` → `posts_tags`) must be rejected.
    #[test]
    fn table_name_collision_rejected() {
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("tags", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                ])
                .build(),
        ];
        let posts_tags = CollectionDefinition::new("posts_tags");

        let registry = Registry::shared();
        registry.write().unwrap().register_collection(posts);
        registry.write().unwrap().register_collection(posts_tags);

        let err = validate_table_name_collisions(&registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("posts_tags"),
            "expected colliding table: {msg}"
        );
    }

    /// Distinct collection and field names generate distinct tables — no error.
    #[test]
    fn table_name_collision_allows_distinct_names() {
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("tags", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                ])
                .build(),
        ];
        let authors = CollectionDefinition::new("authors");

        let registry = Registry::shared();
        registry.write().unwrap().register_collection(posts);
        registry.write().unwrap().register_collection(authors);

        validate_table_name_collisions(&registry.read().unwrap())
            .expect("distinct names must not collide");
    }

    /// Access refs on the collection are checked too.
    #[test]
    fn validate_hook_references_reports_missing_collection_access() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("posts");
        def.access = Access::builder()
            .read(Some(HookRef::new("hooks.gone")))
            .build();
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        let err = validate_hook_references(&lua, &registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("access.read"), "expected kind: {msg}");
        assert!(msg.contains("hooks.gone"), "expected ref: {msg}");
    }

    // ── validate_auth_methods footgun tests ────────────────────────

    use crate::core::collection::{Activation, Auth, AuthMethod, SurfaceSet};

    fn auth_def(slug: &str, methods: Vec<AuthMethod>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.auth = Some(Auth {
            enabled: true,
            methods,
            ..Default::default()
        });
        def
    }

    /// The `mfa = "custom"` mode and its `mfa_deliver` hook come as a pair —
    /// both halves of the pairing are startup errors on their own.
    #[test]
    fn validate_auth_methods_enforces_custom_mfa_deliver_pairing() {
        use crate::core::collection::MfaMode;

        // custom without deliver → error
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(auth_def(
            "users",
            vec![
                AuthMethod::password_login_builder()
                    .mfa(MfaMode::Custom)
                    .build(),
                AuthMethod::bearer(),
            ],
        ));
        let msg = format!(
            "{:#}",
            validate_auth_methods(&registry.read().unwrap()).unwrap_err()
        );
        assert!(msg.contains("requires an mfa_deliver hook"), "{msg}");

        // deliver without custom → error
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(auth_def(
            "users",
            vec![
                AuthMethod::password_login_builder()
                    .mfa(MfaMode::Email)
                    .mfa_deliver(Some(HookRef::new("hooks.mfa.send")))
                    .build(),
                AuthMethod::bearer(),
            ],
        ));
        let msg = format!(
            "{:#}",
            validate_auth_methods(&registry.read().unwrap()).unwrap_err()
        );
        assert!(msg.contains("only valid with mfa = \"custom\""), "{msg}");

        // the valid pair passes
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(auth_def(
            "users",
            vec![
                AuthMethod::password_login_builder()
                    .mfa(MfaMode::Custom)
                    .mfa_deliver(Some(HookRef::new("hooks.mfa.send")))
                    .build(),
                AuthMethod::bearer(),
            ],
        ));
        validate_auth_methods(&registry.read().unwrap()).expect("valid pairing passes");
    }

    #[test]
    fn validate_auth_methods_rejects_empty_surfaces() {
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(auth_def(
            "users",
            vec![
                AuthMethod::password_login(),
                AuthMethod::Bearer {
                    surfaces: SurfaceSet::from_list(vec![]),
                },
            ],
        ));
        let err = validate_auth_methods(&registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("empty `surfaces`"),
            "expected empty-surfaces error: {msg}"
        );
    }

    #[test]
    fn validate_auth_methods_rejects_empty_header_activation() {
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(auth_def(
            "users",
            vec![
                AuthMethod::password_login(),
                AuthMethod::bearer(),
                AuthMethod::Strategy {
                    name: "bogus".to_string(),
                    authenticate: HookRef::new("hooks.auth.bogus"),
                    activates_on: Activation::Header {
                        header: "  ".to_string(),
                    },
                    surfaces: SurfaceSet::admin_only(),
                },
            ],
        ));
        let err = validate_auth_methods(&registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("empty") && msg.contains("activates_on.header"),
            "expected empty-header error: {msg}"
        );
    }

    #[test]
    fn validate_auth_methods_rejects_empty_authenticate_ref() {
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(auth_def(
            "users",
            vec![
                AuthMethod::password_login(),
                AuthMethod::bearer(),
                AuthMethod::Strategy {
                    name: "incomplete".to_string(),
                    authenticate: HookRef::new(""),
                    activates_on: Activation::always(),
                    surfaces: SurfaceSet::admin_only(),
                },
            ],
        ));
        let err = validate_auth_methods(&registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("empty `authenticate`"),
            "expected empty-authenticate error: {msg}"
        );
    }

    #[test]
    fn validate_auth_methods_accepts_well_formed_default_set() {
        let registry = Registry::shared();
        registry
            .write()
            .unwrap()
            .register_collection(auth_def("users", Auth::default_methods()));
        validate_auth_methods(&registry.read().unwrap())
            .expect("default methods should pass validation");
    }

    // ── required_locales validation ──────────────────────────────────────

    fn en_de() -> Vec<String> {
        vec!["en".to_string(), "de".to_string()]
    }

    /// Register a `posts` collection with one field and run the check against
    /// the given locales, returning the result.
    fn check_field(field: FieldDefinition, locales: &[String]) -> Result<()> {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![field];
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);
        validate_required_locales(&registry.read().unwrap(), locales)
    }

    #[test]
    fn required_locales_unknown_code_is_rejected() {
        let err = check_field(
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .required_locales(RequiredLocales::List(vec!["de-DE".to_string()]))
                .build(),
            &en_de(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("de-DE"),
            "error should name the bad code: {err}"
        );
        assert!(err.contains("title"), "error should name the field: {err}");
    }

    #[test]
    fn required_locales_valid_list_passes() {
        check_field(
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .required_locales(RequiredLocales::List(vec![
                    "en".to_string(),
                    "de".to_string(),
                ]))
                .build(),
            &en_de(),
        )
        .expect("configured codes must pass");
    }

    #[test]
    fn required_locales_all_passes() {
        check_field(
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .required_locales(RequiredLocales::All)
                .build(),
            &en_de(),
        )
        .expect("`all` is always valid");
    }

    #[test]
    fn required_locales_none_passes() {
        check_field(
            FieldDefinition::builder("title", FieldType::Text).build(),
            &en_de(),
        )
        .expect("unset is fine");
    }

    #[test]
    fn required_locales_rejected_when_localization_disabled() {
        let err = check_field(
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .required_locales(RequiredLocales::All)
                .build(),
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("localization is not enabled"),
            "should reject required_locales with no configured locales: {err}"
        );
    }

    #[test]
    fn required_locales_collection_level_is_validated() {
        let mut def = CollectionDefinition::new("posts");
        def.required_locales = Some(RequiredLocales::List(vec!["fr".to_string()]));
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .build(),
        ];
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        let err = validate_required_locales(&registry.read().unwrap(), &en_de())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("fr"),
            "collection-level bad code must be caught: {err}"
        );
        assert!(
            err.contains("collection-level"),
            "error should point at the collection-level setting: {err}"
        );
    }

    #[test]
    fn required_locales_validated_inside_groups() {
        let err = check_field(
            FieldDefinition::builder("seo", FieldType::Group)
                .localized(true)
                .fields(vec![
                    FieldDefinition::builder("meta_title", FieldType::Text)
                        .required(true)
                        .required_locales(RequiredLocales::List(vec!["xx".to_string()]))
                        .build(),
                ])
                .build(),
            &en_de(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("xx"), "nested bad code must be caught: {err}");
        assert!(
            err.contains("meta_title"),
            "should name the nested field: {err}"
        );
    }

    /// A required sub-field that inherits localization from a localized group
    /// (without its own `localized = true`) is locale-scoped, so valid
    /// `required_locales` must pass — the per-field parser used to reject this.
    #[test]
    fn required_locales_on_group_inherited_subfield_passes() {
        check_field(
            FieldDefinition::builder("seo", FieldType::Group)
                .localized(true)
                .fields(vec![
                    FieldDefinition::builder("meta_title", FieldType::Text)
                        .required(true)
                        .required_locales(RequiredLocales::List(vec![
                            "en".to_string(),
                            "de".to_string(),
                        ]))
                        .build(),
                ])
                .build(),
            &en_de(),
        )
        .expect("required_locales on a group-inherited sub-field with valid codes must pass");
    }

    /// `required_locales` on a field that is neither localized nor inside a
    /// localized group is meaningless and must be rejected (this check moved
    /// from the per-field parser to startup so it can honor group inheritance).
    #[test]
    fn required_locales_on_non_locale_scoped_field_rejected() {
        let err = check_field(
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .required_locales(RequiredLocales::List(vec!["en".to_string()]))
                .build(),
            &en_de(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("only applies to localized"),
            "non-locale-scoped required_locales must be rejected: {err}"
        );
    }
}
