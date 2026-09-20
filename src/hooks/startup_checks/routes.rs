//! Boot validation of custom routes registered via `crap.routes.register`.

use std::collections::HashSet;

use anyhow::{Result, bail};
use mlua::{Lua, LuaSerdeExt as _, Table, Value};

use crate::admin::custom_routes::is_reserved_path;
use crate::core::HookRef;
use crate::hooks::lifecycle::resolve_hook_function;
use crate::hooks::lua_api::routes::ROUTES_KEY;

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
///
/// # Errors
///
/// Returns an aggregated error listing every route problem found.
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
        check_route_entry(lua, &entry, prefix, &mut seen, &mut errors);
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

/// Validate one registered route entry: mounted path, method/path uniqueness,
/// and its handler / gated-access refs.
fn check_route_entry(
    lua: &Lua,
    entry: &Table,
    prefix: &str,
    seen: &mut HashSet<(String, String)>,
    errors: &mut Vec<String>,
) {
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

    check_route_ref(lua, entry, "handler", &label, errors);

    // `access` is a ref only when gated (not nil / true / false).
    if let Ok(v @ (Value::String(_) | Value::Table(_))) = entry.get::<Value>("access") {
        check_resolvable(lua, v, "access", &label, errors);
    }
}

/// Resolve a required ref field (`handler`) on a route entry.
fn check_route_ref(lua: &Lua, entry: &Table, field: &str, label: &str, out: &mut Vec<String>) {
    match entry.get::<Value>(field) {
        Ok(v) => check_resolvable(lua, v, field, label, out),
        Err(_) => out.push(format!("route '{label}': missing {field}")),
    }
}

/// Decode a value as a `HookRef` and confirm it resolves to a Lua function.
pub(super) fn check_resolvable(
    lua: &Lua,
    value: Value,
    field: &str,
    label: &str,
    out: &mut Vec<String>,
) {
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
