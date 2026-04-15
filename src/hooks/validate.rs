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

use anyhow::{Result, bail};
use mlua::Lua;

use crate::core::{
    FieldDefinition, SharedRegistry,
    collection::{Access, Hooks},
};
use crate::hooks::lifecycle::resolve_hook_function;

/// Validate every statically-known hook and access reference in the registry.
///
/// Returns `Ok(())` when every ref resolves cleanly. Returns `Err` with a
/// single aggregated message listing every unresolved ref and its source.
///
/// Must be called after `init_lua` so `require(...)` can locate modules
/// under `{config_dir}/hooks/` (path configured by `setup_package_paths`).
pub fn validate_hook_references(lua: &Lua, registry: &SharedRegistry) -> Result<()> {
    let mut missing: Vec<String> = Vec::new();

    let Ok(reg) = registry.read() else {
        bail!("Registry lock poisoned during hook validation");
    };

    for (slug, def) in &reg.collections {
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

    for (slug, def) in &reg.globals {
        check_hooks(lua, &def.hooks, &format!("global '{slug}'"), &mut missing);
        check_access(lua, &def.access, &format!("global '{slug}'"), &mut missing);
        check_field_list(lua, &def.fields, &format!("global '{slug}'"), &mut missing);
    }

    if missing.is_empty() {
        return Ok(());
    }

    let body = missing.join("\n  - ");
    bail!(
        "Unresolved hook/access references at startup:\n  - {}\n\n\
         Each line shows `source: kind: 'ref'`. Either create the Lua module/function, \
         fix the typo, or remove the reference from the definition.",
        body
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

/// Reject field names that collide with the generated locale-suffixed column
/// pattern `{field}__{locale}`. If a user defines a literal field named
/// `title__en` while `en` is a configured locale, the generated localized
/// column for `title` would be `title__en` — a silent collision. Fail startup.
pub fn validate_locale_field_collisions(
    registry: &SharedRegistry,
    locales: &[String],
) -> Result<()> {
    if locales.is_empty() {
        return Ok(());
    }

    let Ok(reg) = registry.read() else {
        bail!("Registry lock poisoned during locale-collision validation");
    };

    let mut collisions: Vec<String> = Vec::new();

    for (slug, def) in &reg.collections {
        walk_fields_for_collisions(
            &def.fields,
            locales,
            &format!("collection '{slug}'"),
            &mut collisions,
        );
    }

    for (slug, def) in &reg.globals {
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
        "Field name collides with locale-suffixed column pattern '{{name}}__{{locale}}':\n  - {}\n\n\
         Rename the field (or change its locale suffix) to avoid a silent collision \
         with the generated localized column.",
        body
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

    use crate::core::{
        CollectionDefinition, FieldDefinition, FieldType, Registry,
        collection::{Access, Hooks},
    };

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

        let err = validate_hook_references(&lua, &registry).unwrap_err();
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

        let err = validate_hook_references(&lua, &registry).unwrap_err();
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

        validate_hook_references(&lua, &registry).expect("no refs means no errors");
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
        let err = validate_locale_field_collisions(&registry, &locales).unwrap_err();
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

        validate_locale_field_collisions(&registry, &[]).expect("no locales = no check");
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
        validate_locale_field_collisions(&registry, &locales)
            .expect("no collision when suffix does not match an enabled locale");
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

        let err = validate_hook_references(&lua, &registry).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("access.read"), "expected kind: {msg}");
        assert!(msg.contains("hooks.gone"), "expected ref: {msg}");
    }
}
