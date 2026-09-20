//! Custom pages registered via `crap.pages.register` carry an optional
//! `access` gate ref; a typo there passes registration and locks the page at
//! its first request, so it is resolved at boot like a route's gate.

use anyhow::{Result, bail};
use mlua::{Lua, Table, Value};

use crate::hooks::lua_api::pages::PAGES_KEY;

use super::routes::check_resolvable;

/// Resolve every registered custom page's `access` ref.
///
/// # Errors
///
/// Returns an error naming each page whose gate does not resolve.
pub fn validate_pages(lua: &Lua) -> Result<()> {
    let Ok(pages): mlua::Result<Table> = lua.named_registry_value(PAGES_KEY) else {
        return Ok(());
    };

    let mut errors: Vec<String> = Vec::new();

    for pair in pages.pairs::<String, Table>() {
        let Ok((slug, entry)) = pair else { continue };
        let label = format!("page '{slug}'");

        if let Ok(v @ (Value::String(_) | Value::Table(_))) = entry.get::<Value>("access") {
            check_resolvable(lua, v, "access", &label, &mut errors);
        }
    }

    if errors.is_empty() {
        return Ok(());
    }

    bail!(
        "Custom page access reference validation failed:\n  {}\n\n\
         Create the Lua access module, fix the typo, or remove the gate.",
        errors.join("\n  ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua_with_page(access: &str) -> Lua {
        let lua = Lua::new();
        lua.load(format!(
            "return {{ reports = {{ label = 'Reports'{access} }} }}"
        ))
        .eval::<Table>()
        .map(|pages| lua.set_named_registry_value(PAGES_KEY, pages))
        .expect("page table")
        .expect("registry");
        lua
    }

    #[test]
    fn a_page_without_a_gate_passes() {
        assert!(validate_pages(&lua_with_page("")).is_ok());
    }

    /// A gate naming a module that does not exist used to pass registration
    /// and lock the page at its first request.
    #[test]
    fn a_page_whose_gate_does_not_resolve_fails_the_boot() {
        let err = validate_pages(&lua_with_page(", access = 'hooks.nope'"))
            .expect_err("unresolvable gate");
        let text = err.to_string();
        assert!(
            text.contains("page 'reports'") && text.contains("hooks.nope"),
            "{text}"
        );
    }
}
