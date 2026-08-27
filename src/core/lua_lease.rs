//! Seam between `core` providers (custom email / storage) and the `hooks`
//! Lua VM pool.
//!
//! A custom provider must call user-registered Lua functions
//! (`crap._email_send`, `crap._storage.*`), but `core` must not depend on
//! the `hooks` VM-pool machinery. `LuaVmLease` is that seam: `core`
//! defines and consumes it; `hooks` implements the pooled variant
//! (`impl LuaVmLease for VmPool`, for external callers like the scheduler
//! and HTTP handlers), while [`LocalLease`] here wraps a single VM handle
//! for use *inside* a pool VM — where re-acquiring from the pool would
//! risk deadlock.

use anyhow::Result;
use mlua::{Lua, WeakLua};

/// Lease a Lua VM for the duration of a closure.
///
/// Implementations decide where the VM comes from. The closure carries
/// its own output via captured state and returns `Ok(())` or an error.
pub trait LuaVmLease: Send + Sync {
    /// Run `f` with a borrowed VM.
    ///
    /// # Errors
    ///
    /// Propagates any error from acquiring a VM or from `f` itself.
    fn with_vm(&self, f: &mut dyn FnMut(&Lua) -> Result<()>) -> Result<()>;
}

/// A lease backed by one fixed VM, held as a **weak** handle.
///
/// Used when the caller is already executing inside a pool VM (e.g. a
/// `crap.email.send` from a hook): reusing the current VM avoids
/// re-entrant pool acquisition.
///
/// The handle is weak on purpose. A `LocalLease` is stored *inside* the
/// very VM it points at (inside the `LuaVmInfra` app-data, or the
/// `EmailState` captured by the `crap.email.*` closures). A strong `Lua`
/// back-reference would form a cycle (`RawLua → provider → LocalLease →
/// Lua → RawLua`) that pins the VM forever — leaking pool VMs and, worse,
/// defeating the explicit `drop(lua)` of the init VM (which would keep a
/// writeable registry handle alive). The VM's real owner — the VM pool's
/// `Vec<Lua>` (or the init VM's stack binding) — always outlives any use
/// of the lease, so the upgrade below never fails in practice.
pub struct LocalLease {
    lua: WeakLua,
}

impl LocalLease {
    /// Capture a weak handle to `lua`.
    #[must_use]
    pub fn new(lua: &Lua) -> Self {
        Self { lua: lua.weak() }
    }
}

impl LuaVmLease for LocalLease {
    fn with_vm(&self, f: &mut dyn FnMut(&Lua) -> Result<()>) -> Result<()> {
        let lua = self
            .lua
            .try_upgrade()
            .ok_or_else(|| anyhow::anyhow!("the Lua VM backing this provider has been dropped"))?;
        f(&lua)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn local_lease_runs_against_the_wrapped_vm() {
        let lua = Lua::new();
        lua.globals().set("answer", 42i64).unwrap();
        let lease: Arc<dyn LuaVmLease> = Arc::new(LocalLease::new(&lua));

        let mut seen = 0i64;
        lease
            .with_vm(&mut |lua| {
                seen = lua.globals().get::<i64>("answer")?;
                Ok(())
            })
            .unwrap();

        assert_eq!(seen, 42);
    }

    #[test]
    fn local_lease_propagates_closure_errors() {
        let lua = Lua::new();
        let lease: Arc<dyn LuaVmLease> = Arc::new(LocalLease::new(&lua));
        let err = lease
            .with_vm(&mut |_| Err(anyhow::anyhow!("boom")))
            .unwrap_err();
        assert!(err.to_string().contains("boom"));
    }

    /// The weak handle must break the cycle: once the owning `Lua` is
    /// dropped, the lease can no longer upgrade and reports it instead of
    /// keeping the VM alive forever.
    #[test]
    fn local_lease_errors_after_owner_dropped() {
        let lease: Arc<dyn LuaVmLease> = {
            let lua = Lua::new();
            Arc::new(LocalLease::new(&lua))
            // `lua` (the only strong owner) drops here.
        };
        let err = lease.with_vm(&mut |_| Ok(())).unwrap_err();
        assert!(err.to_string().contains("dropped"), "{err}");
    }
}
