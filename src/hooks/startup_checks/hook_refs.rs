//! Post-registry validation of hook and access references.
//!
//! Operators write hook/access refs as plain strings in collection, global,
//! and field definitions. Without this pass, a typo like
//! `"hooks.field_hooks.slugifyy"` only surfaces at the first request that
//! triggers the hook. This module walks the registry at startup, attempts to
//! resolve every statically-known ref against the init-time Lua VM, and
//! returns a single aggregated error listing every unresolved ref with its
//! source location.
//!
//! Scope: EVERY `HookRef` the definition model can carry — collection +
//! global hooks, access rules, live filters, field hooks, field access,
//! `required_when` / `validate` predicates, field display conditions, job
//! handler/access refs, and auth method refs (strategy `authenticate`,
//! `mfa_when`, `mfa_deliver`) — plus the `[admin] access` config gate. Only
//! dynamic registrations (via `crap.hooks.register`, which passes a live
//! function rather than a string ref) have nothing to validate here. The pin
//! test at the bottom scans `src/core` for `HookRef`-typed keys and fails when
//! one is not on this pass's inventory.

use std::fmt::Write as _;

use anyhow::{Result, bail};
use mlua::Lua;

use crate::core::{
    Access, FieldAccess, FieldDefinition, FieldHooks, HookRef, Hooks, LiveSetting, Registry,
    SchemaStep, collection::AuthMethod, walk_all_fields,
};
use crate::hooks::lifecycle::resolve_hook_function;

/// Validate every statically-known hook and access reference in the registry.
///
/// Returns `Ok(())` when every ref resolves cleanly. Returns `Err` with a
/// single aggregated message listing every unresolved ref and its source.
///
/// Must be called after `init_lua` so `require(...)` can locate modules
/// under `{config_dir}/hooks/` (path configured by `setup_package_paths`).
///
/// # Errors
///
/// Returns an aggregated error naming every unresolved ref and its source.
pub fn validate_hook_references(lua: &Lua, registry: &Registry) -> Result<()> {
    let mut missing: Vec<String> = Vec::new();

    check_collections(lua, registry, &mut missing);
    check_globals(lua, registry, &mut missing);
    check_jobs(lua, registry, &mut missing);

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

/// Record `label: 'ref'` when `maybe_ref` is set and does not resolve to a Lua
/// function. The one place a ref is turned into a report line, so every kind
/// of ref is reported in the same shape.
fn check_ref(lua: &Lua, maybe_ref: Option<&HookRef>, label: &str, out: &mut Vec<String>) {
    let Some(r) = maybe_ref else { return };

    if resolve_hook_function(lua, r.reference()).is_err() {
        out.push(format!("{label}: '{}'", r.reference()));
    }
}

/// Every ref a collection carries: hooks, access, the live filter, its field
/// tree, and its auth methods.
fn check_collections(lua: &Lua, registry: &Registry, out: &mut Vec<String>) {
    for (slug, def) in &registry.collections {
        let source = format!("collection '{slug}'");

        check_hooks(lua, &def.hooks, &source, out);
        check_access(lua, &def.access, &source, out);
        check_live(lua, def.live.as_ref(), &source, out);
        check_field_list(lua, &def.fields, &source, out);

        // Auth method refs: a strategy's `authenticate` function and the
        // `mfa_when` / `mfa_deliver` hooks. `validate_auth_methods` checks
        // their *shape*; this resolves them, so a typo fails to boot instead
        // of stranding every login / MFA attempt at runtime.
        if let Some(auth) = &def.auth {
            check_auth_method_refs(lua, &auth.methods, &source, out);
        }
    }
}

/// Every ref a global carries. Globals have no auth methods; everything else
/// is checked on the same terms as a collection.
fn check_globals(lua: &Lua, registry: &Registry, out: &mut Vec<String>) {
    for (slug, def) in &registry.globals {
        let source = format!("global '{slug}'");

        check_hooks(lua, &def.hooks, &source, out);
        check_access(lua, &def.access, &source, out);
        check_live(lua, def.live.as_ref(), &source, out);
        check_field_list(lua, &def.fields, &source, out);
    }
}

/// Job handler + access refs resolve through the same `resolve_hook_function`
/// mechanism as collection hooks, and jobs are init-only (the registry is
/// immutable post-boot) — so a typo'd handler would otherwise only surface at
/// the first scheduled run.
fn check_jobs(lua: &Lua, registry: &Registry, out: &mut Vec<String>) {
    for (slug, def) in &registry.jobs {
        check_ref(
            lua,
            Some(&def.handler),
            &format!("job '{slug}': handler"),
            out,
        );
        check_ref(
            lua,
            def.access.as_ref(),
            &format!("job '{slug}': access"),
            out,
        );
    }
}

/// Collect any unresolved refs in a `Hooks` struct.
fn check_hooks(lua: &Lua, hooks: &Hooks, source: &str, out: &mut Vec<String>) {
    // Exhaustive destructuring: a new hook slot on
    // `Hooks` fails to compile HERE until this validator learns it.
    let Hooks {
        before_validate,
        before_change,
        after_change,
        before_read,
        after_read,
        before_delete,
        after_delete,
        before_broadcast,
    } = hooks;

    let pairs: [(&str, &[HookRef]); 8] = [
        ("before_validate", before_validate),
        ("before_change", before_change),
        ("after_change", after_change),
        ("before_read", before_read),
        ("after_read", after_read),
        ("before_delete", before_delete),
        ("after_delete", after_delete),
        ("before_broadcast", before_broadcast),
    ];

    for (kind, refs) in pairs {
        for r in refs {
            check_ref(lua, Some(r), &format!("{source}: {kind}"), out);
        }
    }
}

/// Collect any unresolved refs in an `Access` struct.
fn check_access(lua: &Lua, access: &Access, source: &str, out: &mut Vec<String>) {
    // Exhaustive destructuring: a new access key on
    // `Access` fails to compile HERE until this validator learns it.
    let Access {
        read,
        create,
        update,
        delete,
        trash,
        draft,
        versions,
        unlock,
        admin,
        mcp,
    } = access;

    let pairs: [(&str, Option<&HookRef>); 10] = [
        ("access.read", read.as_ref()),
        ("access.create", create.as_ref()),
        ("access.update", update.as_ref()),
        ("access.delete", delete.as_ref()),
        ("access.trash", trash.as_ref()),
        ("access.draft", draft.as_ref()),
        ("access.versions", versions.as_ref()),
        ("access.unlock", unlock.as_ref()),
        ("access.admin", admin.as_ref()),
        ("access.mcp", mcp.as_ref()),
    ];

    for (kind, maybe_ref) in pairs {
        check_ref(lua, maybe_ref, &format!("{source}: {kind}"), out);
    }
}

/// Collect the unresolved ref carried by a `live` setting.
///
/// An unresolvable filter is not a per-event warning to live with: the
/// broadcast path can only log and drop, so every live event for the
/// collection vanishes for the life of the deployment.
fn check_live(lua: &Lua, live: Option<&LiveSetting>, source: &str, out: &mut Vec<String>) {
    let Some(live) = live else { return };

    // Exhaustive on purpose: a new `LiveSetting` variant fails to compile
    // HERE until this validator decides whether it carries a resolvable ref.
    match live {
        LiveSetting::Disabled => {}
        LiveSetting::Function(r) => {
            check_ref(lua, Some(r), &format!("{source}: live.filter"), out);
        }
    }
}

/// Resolve the Lua refs carried by auth methods: `Strategy.authenticate` and
/// `PasswordLogin`'s `mfa_when` / `mfa_deliver`. The other method kinds carry
/// no refs.
fn check_auth_method_refs(lua: &Lua, methods: &[AuthMethod], source: &str, out: &mut Vec<String>) {
    for m in methods {
        // Exhaustive on purpose: a new AuthMethod
        // variant fails to compile HERE until this validator decides
        // whether it carries resolvable refs.
        match m {
            AuthMethod::Strategy {
                name, authenticate, ..
            } => check_ref(
                lua,
                Some(authenticate),
                &format!("{source}: auth strategy '{name}' authenticate"),
                out,
            ),
            AuthMethod::PasswordLogin {
                mfa_when,
                mfa_deliver,
                ..
            } => {
                check_ref(lua, mfa_when.as_ref(), &format!("{source}: mfa_when"), out);
                check_ref(
                    lua,
                    mfa_deliver.as_ref(),
                    &format!("{source}: mfa_deliver"),
                    out,
                );
            }
            AuthMethod::Bearer { .. } | AuthMethod::SessionCookie { .. } => {}
        }
    }
}

/// Resolve the `[admin] access` config gate ref. The runtime gate fails
/// CLOSED on an unresolvable ref — correct, but that means a typo locks
/// every user (the operator included) out of the admin panel, so the boot
/// fails with the ref named instead.
///
/// # Errors
///
/// Returns `Err` naming the ref when it does not resolve.
pub fn validate_admin_access_ref(lua: &Lua, admin_access: Option<&HookRef>) -> Result<()> {
    let Some(r) = admin_access else {
        return Ok(());
    };

    if resolve_hook_function(lua, r.reference()).is_err() {
        bail!(
            "[admin] access = '{}' does not resolve to a Lua function — \
             the admin gate fails closed, so this would lock everyone out \
             of the admin panel. Fix the ref or remove the setting.",
            r.reference()
        );
    }

    Ok(())
}

/// Render the `{source} field 'a' block 'b' …` source label for a field from
/// its [`SchemaStep`] ancestor chain.
pub(super) fn field_source_label(
    source: &str,
    path: &[SchemaStep<'_>],
    field: &FieldDefinition,
) -> String {
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
/// and collect every unresolved field-level reference.
fn check_field_list(lua: &Lua, fields: &[FieldDefinition], source: &str, out: &mut Vec<String>) {
    walk_all_fields(fields, &mut Vec::new(), &mut |f, path| {
        check_one_field(lua, f, &field_source_label(source, path, f), out);
    });
}

/// Collect every unresolved ref one field carries: lifecycle hooks, access
/// rules, the `required_when` / `validate` predicates, and the admin display
/// condition.
fn check_one_field(lua: &Lua, f: &FieldDefinition, field_src: &str, out: &mut Vec<String>) {
    // Exhaustive destructuring: a new field-hook slot fails to compile HERE.
    let FieldHooks {
        before_validate,
        before_change,
        after_change,
        after_read,
    } = &f.hooks;

    let hook_pairs: [(&str, &[HookRef]); 4] = [
        ("before_validate", before_validate),
        ("before_change", before_change),
        ("after_change", after_change),
        ("after_read", after_read),
    ];

    for (kind, refs) in hook_pairs {
        for r in refs {
            check_ref(lua, Some(r), &format!("{field_src}: {kind}"), out);
        }
    }

    // Exhaustive destructuring: a new field-access key fails to compile HERE.
    let FieldAccess {
        read,
        create,
        update,
    } = &f.access;

    let access_pairs: [(&str, Option<&HookRef>); 3] = [
        ("access.read", read.as_ref()),
        ("access.create", create.as_ref()),
        ("access.update", update.as_ref()),
    ];

    for (kind, maybe_ref) in access_pairs {
        check_ref(lua, maybe_ref, &format!("{field_src}: {kind}"), out);
    }

    // Validation predicates and the display condition are always function
    // refs (inline tables are not accepted on any of them).
    check_ref(
        lua,
        f.required_when.as_ref(),
        &format!("{field_src}: required_when"),
        out,
    );
    check_ref(
        lua,
        f.validate.as_ref(),
        &format!("{field_src}: validate"),
        out,
    );
    check_ref(
        lua,
        f.admin.condition.as_ref(),
        &format!("{field_src}: admin.condition"),
        out,
    );
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs::{read_dir, read_to_string};
    use std::path::Path;

    use mlua::{Lua, LuaOptions, StdLib};

    use crate::core::{
        CollectionDefinition, FieldDefinition, FieldType, GlobalDefinition, Registry,
        collection::{Activation, Auth, AuthMethod, MfaMode, SurfaceSet},
        job::JobDefinition,
    };

    use super::*;

    fn sandboxed_lua() -> Lua {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).unwrap();
        crate::hooks::sandbox_lua(&lua).unwrap();
        lua
    }

    /// Register one collection and run the reference check, returning the
    /// aggregated message.
    fn collection_error(lua: &Lua, def: CollectionDefinition) -> String {
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        let err = validate_hook_references(lua, &registry.read().unwrap()).unwrap_err();
        format!("{err:#}")
    }

    /// Missing ref surfaces at startup with collection + kind in the message.
    #[test]
    fn validate_hook_references_reports_missing_collection_hook() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("posts");
        def.hooks = Hooks::builder()
            .before_change(vec![HookRef::new("hooks.missing.module")])
            .build();

        let msg = collection_error(&lua, def);
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

        let msg = collection_error(&lua, def);
        assert!(msg.contains("title"), "expected field name: {msg}");
        assert!(msg.contains("access.read"), "expected kind: {msg}");
        assert!(msg.contains("hooks.never.exists"), "expected ref: {msg}");
    }

    /// Regression: a `live.filter` ref was never resolved at boot. A typo'd
    /// one passes startup and the broadcast path can only warn and drop —
    /// every live event for the collection vanishes silently.
    #[test]
    fn validate_hook_references_reports_missing_collection_live_filter() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("posts");
        def.live = Some(LiveSetting::Function(HookRef::new("hooks.live.publishedd")));

        let msg = collection_error(&lua, def);
        assert!(msg.contains("collection 'posts'"), "expected slug: {msg}");
        assert!(msg.contains("live.filter"), "expected kind: {msg}");
        assert!(msg.contains("hooks.live.publishedd"), "expected ref: {msg}");
    }

    /// A global's `live.filter` is checked on the same terms as a collection's.
    #[test]
    fn validate_hook_references_reports_missing_global_live_filter() {
        let lua = sandboxed_lua();
        let mut def = GlobalDefinition::new("settings");
        def.live = Some(LiveSetting::Function(HookRef::new("hooks.live.nope")));

        let registry = Registry::shared();
        registry.write().unwrap().register_global(def);

        let err = validate_hook_references(&lua, &registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("global 'settings'"), "expected slug: {msg}");
        assert!(msg.contains("live.filter"), "expected kind: {msg}");
        assert!(msg.contains("hooks.live.nope"), "expected ref: {msg}");
    }

    /// `live = false` (Disabled) carries no ref and validates cleanly.
    #[test]
    fn validate_hook_references_accepts_disabled_live() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("posts");
        def.live = Some(LiveSetting::Disabled);

        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        validate_hook_references(&lua, &registry.read().unwrap())
            .expect("a disabled live setting has no ref to resolve");
    }

    /// A job whose handler ref does not resolve fails the boot with the
    /// job slug and ref named. (Jobs previously escaped validation on the
    /// stale assumption that they resolve through a different mechanism —
    /// they use the same `resolve_hook_function` as collection hooks.)
    #[test]
    fn validate_hook_references_reports_missing_job_handler() {
        let lua = sandboxed_lua();
        let registry = Registry::shared();
        registry
            .write()
            .unwrap()
            .register_job(JobDefinition::builder("cleanup", "jobs.cleanup.runn").build());

        let err = validate_hook_references(&lua, &registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("job 'cleanup'"), "expected job slug: {msg}");
        assert!(msg.contains("handler"), "expected kind: {msg}");
        assert!(msg.contains("jobs.cleanup.runn"), "expected ref: {msg}");
    }

    /// A job access ref that does not resolve is reported too.
    #[test]
    fn validate_hook_references_reports_missing_job_access() {
        let lua = sandboxed_lua();
        let mut job = JobDefinition::builder("cleanup", "jobs.cleanup.runn").build();
        job.access = Some(HookRef::new("hooks.access.adminz"));
        let registry = Registry::shared();
        registry.write().unwrap().register_job(job);

        let err = validate_hook_references(&lua, &registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("job 'cleanup': access: 'hooks.access.adminz'"),
            "expected job access line: {msg}"
        );
    }

    /// A strategy `authenticate` ref that does not resolve fails the boot.
    #[test]
    fn validate_hook_references_reports_missing_strategy_authenticate() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("users");
        let mut auth = Auth::enabled();
        auth.methods.push(AuthMethod::Strategy {
            name: "api-key".to_string(),
            authenticate: HookRef::new("hooks.auth.api_keyy"),
            activates_on: Activation::Always { always: true },
            surfaces: SurfaceSet::all(),
        });
        def.auth = Some(auth);

        let msg = collection_error(&lua, def);
        assert!(
            msg.contains("auth strategy 'api-key'"),
            "expected strategy name: {msg}"
        );
        assert!(msg.contains("hooks.auth.api_keyy"), "expected ref: {msg}");
    }

    /// An `mfa_deliver` ref that does not resolve fails the boot.
    #[test]
    fn validate_hook_references_reports_missing_mfa_deliver() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("users");
        let mut auth = Auth::enabled();
        for m in &mut auth.methods {
            if let AuthMethod::PasswordLogin {
                mfa, mfa_deliver, ..
            } = m
            {
                *mfa = MfaMode::Custom;
                *mfa_deliver = Some(HookRef::new("hooks.send_smss"));
            }
        }
        def.auth = Some(auth);

        let msg = collection_error(&lua, def);
        assert!(msg.contains("mfa_deliver"), "expected kind: {msg}");
        assert!(msg.contains("hooks.send_smss"), "expected ref: {msg}");
    }

    /// Regression: the `mfa_when` gate ref was never resolved at boot. The
    /// gate fails closed on a call error, so a typo silently forced MFA on
    /// every login instead of failing the boot.
    #[test]
    fn validate_hook_references_reports_missing_mfa_when() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("users");
        let mut auth = Auth::enabled();
        for m in &mut auth.methods {
            if let AuthMethod::PasswordLogin { mfa_when, .. } = m {
                *mfa_when = Some(HookRef::new("hooks.mfa.when_riskyy"));
            }
        }
        def.auth = Some(auth);

        let msg = collection_error(&lua, def);
        assert!(msg.contains("mfa_when"), "expected kind: {msg}");
        assert!(msg.contains("hooks.mfa.when_riskyy"), "expected ref: {msg}");
    }

    /// A field display-condition ref that does not resolve fails the boot.
    #[test]
    fn validate_hook_references_reports_missing_condition() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("posts");
        let mut field = FieldDefinition::builder("url", FieldType::Text).build();
        field.admin.condition = Some(HookRef::new("hooks.conditions.show_urll"));
        def.fields = vec![field];

        let msg = collection_error(&lua, def);
        assert!(msg.contains("admin.condition"), "expected kind: {msg}");
        assert!(
            msg.contains("hooks.conditions.show_urll"),
            "expected ref: {msg}"
        );
    }

    /// Regression: the `required_when` predicate and the custom `validate`
    /// ref were never resolved at boot — a typo in either only surfaced as a
    /// per-write failure on the first save.
    #[test]
    fn validate_hook_references_reports_missing_validation_predicates() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("posts");
        let mut field = FieldDefinition::builder("qty", FieldType::Number).build();
        field.required_when = Some(HookRef::new("hooks.validate.needs_qtyy"));
        field.validate = Some(HookRef::new("hooks.validate.in_rangee"));
        def.fields = vec![field];

        let msg = collection_error(&lua, def);
        assert!(msg.contains("required_when"), "expected kind: {msg}");
        assert!(
            msg.contains("hooks.validate.needs_qtyy"),
            "expected ref: {msg}"
        );
        assert!(msg.contains("validate"), "expected kind: {msg}");
        assert!(
            msg.contains("hooks.validate.in_rangee"),
            "expected ref: {msg}"
        );
    }

    /// A typo'd `[admin] access` ref fails the boot instead of locking
    /// everyone out at runtime (the gate fails closed).
    #[test]
    fn validate_admin_access_ref_reports_missing_ref() {
        let lua = sandboxed_lua();
        let r = HookRef::new("access.admin_panle");

        let err = validate_admin_access_ref(&lua, Some(&r)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("access.admin_panle"), "expected ref: {msg}");
        assert!(msg.contains("lock"), "expected consequence: {msg}");

        validate_admin_access_ref(&lua, None).expect("absent gate is fine");
    }

    /// Access refs on the collection are checked too.
    #[test]
    fn validate_hook_references_reports_missing_collection_access() {
        let lua = sandboxed_lua();
        let mut def = CollectionDefinition::new("posts");
        def.access = Access::builder()
            .read(Some(HookRef::new("hooks.gone")))
            .build();

        let msg = collection_error(&lua, def);
        assert!(msg.contains("access.read"), "expected kind: {msg}");
        assert!(msg.contains("hooks.gone"), "expected ref: {msg}");
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

    // ── inventory pin ────────────────────────────────────────────────────

    /// Every `HookRef`-carrying key of the definition model that this pass
    /// resolves. Keys are spelled exactly as `src/core` declares them: struct
    /// field names, plus `Function` for the `LiveSetting::Function(HookRef)`
    /// variant that carries the `live.filter` ref.
    const VALIDATED_HOOK_REF_KEYS: &[&str] = &[
        "Function",
        "access",
        "admin",
        "after_change",
        "after_delete",
        "after_read",
        "authenticate",
        "before_broadcast",
        "before_change",
        "before_delete",
        "before_read",
        "before_validate",
        "condition",
        "create",
        "delete",
        "draft",
        "handler",
        "mcp",
        "mfa_deliver",
        "mfa_when",
        "read",
        "required_when",
        "trash",
        "unlock",
        "update",
        "validate",
        "versions",
    ];

    /// Does a field's type spelling name a `HookRef` (bare, optional, list)?
    fn is_hook_ref_type(tail: &str) -> bool {
        let ty = tail.trim().trim_end_matches(',').trim();

        matches!(ty, "HookRef" | "Option<HookRef>" | "Vec<HookRef>")
    }

    /// The `HookRef` key a source line declares, if any: a struct/enum field
    /// (`name: Option<HookRef>`) or a tuple variant (`Variant(HookRef),`).
    fn hook_ref_key(line: &str) -> Option<String> {
        let line = line.trim();

        if line.starts_with("//") {
            return None;
        }

        if let Some(variant) = line.strip_suffix("(HookRef),") {
            return Some(variant.to_string());
        }

        let (head, tail) = line.split_once(": ")?;

        if !is_hook_ref_type(tail) {
            return None;
        }

        let key = head.trim_start_matches("pub ").trim();

        (!key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
            .then(|| key.to_string())
    }

    /// Scan a `.rs` file, or every `.rs` file under a directory.
    fn collect_hook_ref_keys(root: &Path, out: &mut BTreeSet<String>) {
        if root.is_file() {
            if root.extension().is_some_and(|e| e == "rs") {
                let src = read_to_string(root).unwrap_or_default();
                out.extend(src.lines().filter_map(hook_ref_key));
            }
            return;
        }

        let entries = read_dir(root).unwrap_or_else(|e| panic!("read {}: {e}", root.display()));

        for entry in entries.flatten() {
            collect_hook_ref_keys(&entry.path(), out);
        }
    }

    /// Pin: every `HookRef` the definition model can carry is validated here.
    ///
    /// A source scan of `src/core` is the inventory's counterpart — adding a
    /// new ref key to a definition struct (or a new ref-carrying enum variant)
    /// fails this test until the key is both listed and actually resolved by
    /// the pass above. `live.filter`, `mfa_when`, `required_when` and
    /// `validate` were each in the model but never resolved.
    #[test]
    fn every_hook_ref_key_in_the_model_is_validated() {
        let mut found: BTreeSet<String> = BTreeSet::new();
        for root in ["src/core", "src/admin/custom_pages.rs"] {
            collect_hook_ref_keys(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join(root),
                &mut found,
            );
        }

        assert!(
            found.len() > 20,
            "the source scan found suspiciously few HookRef keys: {found:?}"
        );

        let inventory: BTreeSet<String> = VALIDATED_HOOK_REF_KEYS
            .iter()
            .map(ToString::to_string)
            .collect();

        let unvalidated: Vec<&String> = found.difference(&inventory).collect();
        assert!(
            unvalidated.is_empty(),
            "these HookRef keys exist in src/core but are not validated at startup: \
             {unvalidated:?} — resolve them in this module and add them to \
             VALIDATED_HOOK_REF_KEYS"
        );

        let stale: Vec<&String> = inventory.difference(&found).collect();
        assert!(
            stale.is_empty(),
            "VALIDATED_HOOK_REF_KEYS names keys that no longer exist in src/core: {stale:?}"
        );
    }
}
