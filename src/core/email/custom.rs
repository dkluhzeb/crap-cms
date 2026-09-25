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

use super::{EmailJobData, EmailProvider};
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

/// Call the registered `send` function on `lua` with the email.
fn deliver(lua: &Lua, email: &EmailJobData) -> Result<()> {
    let func = send_fn(lua)?;

    let opts = lua.create_table()?;
    opts.set("to", email.to.as_str())?;
    opts.set("subject", email.subject.as_str())?;
    opts.set("html", email.html.as_str())?;
    if let Some(plain) = &email.text {
        opts.set("text", plain.as_str())?;
    }

    func.call::<()>(opts)
        .map_err(|e| anyhow!("custom email send error: {e:#}"))
}

impl EmailProvider for CustomEmailProvider {
    fn send(&self, to: &str, subject: &str, html: &str, text: Option<&str>) -> Result<()> {
        let email = EmailJobData {
            to: to.to_string(),
            subject: subject.to_string(),
            html: html.to_string(),
            text: text.map(str::to_string),
        };

        self.lease.with_vm(&mut |lua| deliver(lua, &email))
    }

    /// The queued delivery runs the user's `send` under the queue's timeout:
    /// a hung or looping provider stops there instead of holding the email
    /// queue (and the MFA, reset and verification mails behind it).
    fn send_queued(&self, email: &EmailJobData, timeout_secs: u64) -> Result<()> {
        self.lease
            .with_vm_until(timeout_secs, &mut |lua| deliver(lua, email))
    }

    fn kind(&self) -> &'static str {
        "custom"
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

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

    /// Records the timeout a queued delivery asked for, then runs on `lua`.
    struct RecordingLease {
        lua: Lua,
        timeout: Mutex<Option<u64>>,
    }

    impl LuaVmLease for RecordingLease {
        fn with_vm(&self, f: &mut dyn FnMut(&Lua) -> Result<()>) -> Result<()> {
            f(&self.lua)
        }

        fn with_vm_until(
            &self,
            timeout_secs: u64,
            f: &mut dyn FnMut(&Lua) -> Result<()>,
        ) -> Result<()> {
            *self.timeout.lock().unwrap() = Some(timeout_secs);
            f(&self.lua)
        }
    }

    /// Regression: a queued email through the custom provider ran the user's
    /// `send` with no deadline. The queued path now leases its VM bounded by
    /// the queue timeout; the direct `send` keeps the caller's bounds.
    #[test]
    fn queued_delivery_runs_under_the_queue_timeout() {
        let (lua, _) = lease_with_send();
        let lease = Arc::new(RecordingLease {
            lua,
            timeout: Mutex::new(None),
        });
        let provider = CustomEmailProvider::new(lease.clone());
        let email = EmailJobData {
            to: "user@example.com".into(),
            subject: "Code".into(),
            html: "<p>1234</p>".into(),
            text: None,
        };

        provider
            .send("user@example.com", "S", "<p>x</p>", None)
            .unwrap();
        assert_eq!(*lease.timeout.lock().unwrap(), None);

        provider.send_queued(&email, 42).unwrap();
        assert_eq!(*lease.timeout.lock().unwrap(), Some(42));

        let sent: i64 = lease.lua.load("return #crap._sent").eval().unwrap();
        assert_eq!(sent, 2, "both deliveries reached the Lua provider");
    }

    #[test]
    fn kind_returns_custom() {
        let (_lua, lease) = lease_with_send();
        let provider = CustomEmailProvider::new(lease);
        assert_eq!(provider.kind(), "custom");
    }
}
