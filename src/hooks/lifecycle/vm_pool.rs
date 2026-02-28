//! Lua VM pool for concurrent hook execution.

use mlua::Lua;
use std::sync::{Condvar, Mutex};

/// Pool of Lua VMs for concurrent hook execution.
pub(super) struct VmPool {
    vms: Mutex<Vec<Lua>>,
    available: Condvar,
}

impl VmPool {
    pub(super) fn new(vms: Vec<Lua>) -> Self {
        VmPool {
            vms: Mutex::new(vms),
            available: Condvar::new(),
        }
    }

    /// Acquire a VM from the pool, blocking until one is available.
    pub(super) fn acquire(&self) -> std::result::Result<VmGuard<'_>, String> {
        let mut pool = self.vms.lock()
            .map_err(|e| format!("VM pool lock poisoned: {}", e))?;
        loop {
            if let Some(vm) = pool.pop() {
                return Ok(VmGuard { pool: self, vm: Some(vm) });
            }
            pool = self.available.wait(pool)
                .map_err(|e| format!("VM pool condvar wait failed: {}", e))?;
        }
    }
}

/// RAII guard that returns a VM to the pool on drop.
pub(super) struct VmGuard<'a> {
    pool: &'a VmPool,
    vm: Option<Lua>,
}

impl std::ops::Deref for VmGuard<'_> {
    type Target = Lua;
    fn deref(&self) -> &Lua {
        self.vm.as_ref().unwrap()
    }
}

impl Drop for VmGuard<'_> {
    fn drop(&mut self) {
        if let Some(vm) = self.vm.take() {
            if let Ok(mut pool) = self.pool.vms.lock() {
                pool.push(vm);
                self.pool.available.notify_one();
            }
        }
    }
}
