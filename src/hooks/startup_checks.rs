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

use std::collections::HashMap;

use anyhow::{Result, bail};
use mlua::Lua;
use tracing::warn;

use crate::core::{
    Access, FieldDefinition, FieldType, Hooks, Registry, Slug,
    collection::{Activation, AuthMethod, Surface},
};
use crate::db::query::helpers::{global_table, join_table, prefixed_name};
use crate::hooks::lifecycle::resolve_hook_function;

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

/// Collect any unresolved refs in a `Hooks` struct.
fn check_hooks(lua: &Lua, hooks: &Hooks, source: &str, out: &mut Vec<String>) {
    let pairs: [(&str, &[String]); 8] = [
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
            if resolve_hook_function(lua, r).is_err() {
                out.push(format!("{source}: {kind}: '{r}'"));
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
            AuthMethod::PasswordLogin { .. } => password_count += 1,
            AuthMethod::Bearer { .. } => bearer_count += 1,
            AuthMethod::Strategy {
                name,
                authenticate,
                activates_on,
                surfaces,
            } => check_strategy_shape(
                slug,
                name,
                authenticate,
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
    for f in fields {
        for loc in locales {
            let suffix = format!("__{loc}");
            if f.name.ends_with(&suffix) && f.name.len() > suffix.len() {
                out.push(format!(
                    "{source} field '{}': ends with locale suffix '{}'",
                    f.name, suffix
                ));
            }
        }

        if !f.fields.is_empty() {
            walk_fields_for_collisions(&f.fields, locales, source, out);
        }
        for block in &f.blocks {
            walk_fields_for_collisions(&block.fields, locales, source, out);
        }
        for tab in &f.tabs {
            walk_fields_for_collisions(&tab.fields, locales, source, out);
        }
    }
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
        collect_field_tables(slug, &def.fields, "", &mut owners, &mut conflicts);
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
/// the migration orchestrator's traversal (`sync_join_tables_inner`).
fn collect_field_tables(
    slug: &str,
    fields: &[FieldDefinition],
    prefix: &str,
    owners: &mut HashMap<String, String>,
    conflicts: &mut Vec<String>,
) {
    for field in fields {
        let full_name = prefixed_name(prefix, &field.name);

        match field.field_type {
            FieldType::Relationship | FieldType::Upload
                if field.relationship.as_ref().is_some_and(|rc| rc.has_many) =>
            {
                claim_table(
                    owners,
                    conflicts,
                    join_table(slug, &full_name),
                    format!("has-many field '{full_name}' of collection '{slug}'"),
                );
            }
            FieldType::Array | FieldType::Blocks => {
                claim_table(
                    owners,
                    conflicts,
                    join_table(slug, &full_name),
                    format!("field '{full_name}' of collection '{slug}'"),
                );
            }
            FieldType::Group => {
                collect_field_tables(slug, &field.fields, &full_name, owners, conflicts);
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_field_tables(slug, &field.fields, prefix, owners, conflicts);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_field_tables(slug, &tab.fields, prefix, owners, conflicts);
                }
            }
            _ => {}
        }
    }
}

/// Collect any unresolved refs in an `Access` struct.
fn check_access(lua: &Lua, access: &Access, source: &str, out: &mut Vec<String>) {
    let pairs: [(&str, Option<&str>); 5] = [
        ("access.read", access.read.as_deref()),
        ("access.create", access.create.as_deref()),
        ("access.update", access.update.as_deref()),
        ("access.delete", access.delete.as_deref()),
        ("access.trash", access.trash.as_deref()),
    ];

    for (kind, maybe_ref) in pairs {
        let Some(r) = maybe_ref else { continue };
        if resolve_hook_function(lua, r).is_err() {
            out.push(format!("{source}: {kind}: '{r}'"));
        }
    }
}

/// Walk a field list (including layout wrappers, groups, arrays, blocks, tabs)
/// and collect every unresolved field hook / access reference.
fn check_field_list(lua: &Lua, fields: &[FieldDefinition], source: &str, out: &mut Vec<String>) {
    for f in fields {
        let field_src = format!("{source} field '{}'", f.name);

        // field-level hooks
        let hook_pairs: [(&str, &[String]); 4] = [
            ("before_validate", &f.hooks.before_validate),
            ("before_change", &f.hooks.before_change),
            ("after_change", &f.hooks.after_change),
            ("after_read", &f.hooks.after_read),
        ];
        for (kind, refs) in hook_pairs {
            for r in refs {
                if resolve_hook_function(lua, r).is_err() {
                    out.push(format!("{field_src}: {kind}: '{r}'"));
                }
            }
        }

        // field-level access
        let access_pairs: [(&str, Option<&str>); 3] = [
            ("access.read", f.access.read.as_deref()),
            ("access.create", f.access.create.as_deref()),
            ("access.update", f.access.update.as_deref()),
        ];
        for (kind, maybe_ref) in access_pairs {
            let Some(r) = maybe_ref else { continue };
            if resolve_hook_function(lua, r).is_err() {
                out.push(format!("{field_src}: {kind}: '{r}'"));
            }
        }

        // Recurse into subtrees: sub-fields, block sub-fields, tab sub-fields.
        if !f.fields.is_empty() {
            check_field_list(lua, &f.fields, &field_src, out);
        }
        for block in &f.blocks {
            let block_src = format!("{field_src} block '{}'", block.block_type);
            check_field_list(lua, &block.fields, &block_src, out);
        }
        for (idx, tab) in f.tabs.iter().enumerate() {
            let tab_src = format!("{field_src} tab #{} ('{}')", idx, tab.label);
            check_field_list(lua, &tab.fields, &tab_src, out);
        }
    }
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
            .before_change(vec!["hooks.missing.module".to_string()])
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
        field.access.read = Some("hooks.never.exists".to_string());
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
            .read(Some("hooks.gone".to_string()))
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
                    authenticate: "hooks.auth.bogus".to_string(),
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
                    authenticate: String::new(),
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
}
