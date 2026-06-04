//! Custom Lua-delegated email provider.
//!
//! Delegates email sending to a user-provided Lua function registered via
//! `crap.email.register({ send = function(...) end })`. The VM that runs
//! the function is supplied by a [`LuaVmLease`] — a `LocalLease` when the
//! provider is used from inside a pool VM, or the hook runner's pooled
//! lease for external callers (scheduler, HTTP handlers).

use std::sync::Arc;

use anyhow::{Result, anyhow};
use mlua::{Function, Lua, Table};

use super::EmailProvider;
use crate::core::lua_lease::LuaVmLease;

/// Custom email provider that delegates to a Lua function.
pub struct CustomEmailProvider {
    lease: Arc<dyn LuaVmLease>,
}

impl CustomEmailProvider {
    /// Create a new custom email provider backed by `lease`. The leased
    /// VM must have `crap._email_send` registered (via `init.lua`).
    #[must_use]
    pub fn new(lease: Arc<dyn LuaVmLease>) -> Self {
        Self { lease }
    }
}

/// Look up the registered `crap._email_send` function on a VM.
fn send_fn(lua: &Lua) -> Result<Function> {
    let crap: Table = lua
        .globals()
        .get("crap")
        .map_err(|e| anyhow!("crap global not found: {e}"))?;

    crap.get("_email_send").map_err(|e| {
        anyhow!("crap._email_send not registered — call crap.email.register in init.lua: {e}")
    })
}

impl EmailProvider for CustomEmailProvider {
    fn send(&self, to: &str, subject: &str, html: &str, text: Option<&str>) -> Result<()> {
        self.lease.with_vm(&mut |lua| {
            let func = send_fn(lua)?;

            let opts = lua.create_table()?;
            opts.set("to", to.to_string())?;
            opts.set("subject", subject.to_string())?;
            opts.set("html", html.to_string())?;
            if let Some(plain) = text {
                opts.set("text", plain.to_string())?;
            }

            func.call::<()>(opts)
                .map_err(|e| anyhow!("custom email send error: {e:#}"))?;

            Ok(())
        })
    }

    fn kind(&self) -> &'static str {
        "custom"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lua_lease::LocalLease;

    /// Returns the owning `Lua` alongside the lease — the caller must keep
    /// the VM alive (the lease holds only a weak handle).
    fn lease_with_send() -> (Lua, Arc<dyn LuaVmLease>) {
        let lua = Lua::new();
        lua.load(
            r"
            crap = {}
            local sent = {}
            crap._email_send = function(opts)
                table.insert(sent, opts)
            end
            crap._sent = sent
            ",
        )
        .exec()
        .expect("Lua setup failed");
        let lease: Arc<dyn LuaVmLease> = Arc::new(LocalLease::new(&lua));
        (lua, lease)
    }

    #[test]
    fn send_delegates_to_lua() {
        let (_lua, lease) = lease_with_send();
        let provider = CustomEmailProvider::new(lease);

        provider
            .send("user@example.com", "Test Subject", "<p>Hello</p>", None)
            .unwrap();
    }

    #[test]
    fn send_with_text_body() {
        let (_lua, lease) = lease_with_send();
        let provider = CustomEmailProvider::new(lease);

        provider
            .send(
                "user@example.com",
                "Test",
                "<p>Hello</p>",
                Some("Hello plain"),
            )
            .unwrap();
    }

    #[test]
    fn send_errors_without_function() {
        let lua = Lua::new();
        lua.load("crap = {}").exec().unwrap();
        let provider = CustomEmailProvider::new(Arc::new(LocalLease::new(&lua)));

        let result = provider.send("user@example.com", "Test", "<p>Hi</p>", None);
        assert!(result.is_err());
    }

    #[test]
    fn kind_returns_custom() {
        let (_lua, lease) = lease_with_send();
        let provider = CustomEmailProvider::new(lease);
        assert_eq!(provider.kind(), "custom");
    }
}
