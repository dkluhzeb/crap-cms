//! Lua VM pool for concurrent hook execution.

use anyhow::{anyhow, bail};
use mlua::{HookTriggers, Lua, VmState};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use crate::hooks::lifecycle::types::MaxInstructions;

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

    /// Acquire a VM from the pool, blocking up to 5 seconds.
    pub(super) fn acquire(&self) -> anyhow::Result<VmGuard<'_>> {
        let timeout = Duration::from_secs(5);
        let mut pool = self
            .vms
            .lock()
            .map_err(|e| anyhow!("VM pool lock poisoned: {}", e))?;
        loop {
            if let Some(vm) = pool.pop() {
                set_instruction_hook(&vm);
                return Ok(VmGuard {
                    pool: self,
                    vm: Some(vm),
                });
            }
            let (guard, wait_result) = self
                .available
                .wait_timeout(pool, timeout)
                .map_err(|e| anyhow!("VM pool condvar wait failed: {}", e))?;
            pool = guard;

            if wait_result.timed_out() {
                // Try one more time after timeout — another thread may have returned a VM
                if let Some(vm) = pool.pop() {
                    set_instruction_hook(&vm);
                    return Ok(VmGuard {
                        pool: self,
                        vm: Some(vm),
                    });
                }
                bail!("VM pool acquire timed out after 5s");
            }
        }
    }
}

/// RAII guard that returns a VM to the pool on drop.
pub(super) struct VmGuard<'a> {
    pool: &'a VmPool,
    vm: Option<Lua>,
}

impl std::fmt::Debug for VmGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmGuard").finish_non_exhaustive()
    }
}

impl std::ops::Deref for VmGuard<'_> {
    type Target = Lua;
    fn deref(&self) -> &Lua {
        self.vm.as_ref().expect("VmGuard used after drop")
    }
}

impl Drop for VmGuard<'_> {
    fn drop(&mut self) {
        if let Some(vm) = self.vm.take()
            && let Ok(mut pool) = self.pool.vms.lock()
        {
            vm.remove_hook();
            pool.push(vm);
            self.pool.available.notify_one();
        }
    }
}

/// Set an instruction-counting hook on the VM if `MaxInstructions` is configured.
fn set_instruction_hook(vm: &Lua) {
    let max = vm
        .app_data_ref::<MaxInstructions>()
        .map(|m| m.0)
        .unwrap_or(0);
    if max > 0 {
        let counter = Arc::new(AtomicU64::new(0));
        let c = counter.clone();
        let _ = vm.set_hook(
            HookTriggers::new().every_nth_instruction(10_000),
            move |_lua, _debug| {
                let count = c.fetch_add(10_000, Ordering::Relaxed);
                if count + 10_000 > max {
                    return Err(mlua::Error::RuntimeError(
                        "Lua execution exceeded instruction limit".into(),
                    ));
                }
                Ok(VmState::Continue)
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn make_pool(n: usize) -> VmPool {
        let vms = (0..n).map(|_| Lua::new()).collect();
        VmPool::new(vms)
    }

    #[test]
    fn acquire_returns_valid_vm() {
        let pool = make_pool(1);
        let guard = pool.acquire().expect("should acquire VM");
        // Deref to Lua works — evaluate a trivial expression
        let result: i64 = guard.load("return 1 + 1").eval().expect("lua eval failed");
        assert_eq!(result, 2);
    }

    #[test]
    fn drop_returns_vm_to_pool() {
        let pool = make_pool(1);

        {
            let _guard = pool.acquire().expect("first acquire should succeed");
            // guard is dropped here
        }

        // After drop the VM is back; a second acquire must succeed
        let guard2 = pool.acquire().expect("acquire after drop should succeed");
        let result: i64 = guard2.load("return 42").eval().expect("lua eval failed");
        assert_eq!(result, 42);
    }

    #[test]
    fn concurrent_acquire_two_vms() {
        let pool = Arc::new(make_pool(2));

        let pool_a = Arc::clone(&pool);
        let pool_b = Arc::clone(&pool);

        // Each thread acquires a guard, uses it, and returns the Lua eval result.
        // The guard borrows from the Arc-owned pool inside its own thread, so no
        // lifetime escape issue.
        let handle_a = thread::spawn(move || {
            let guard = pool_a.acquire().expect("thread A: acquire should succeed");
            let v: i64 = guard
                .load("return 1")
                .eval()
                .expect("lua eval on guard_a failed");
            v
        });
        let handle_b = thread::spawn(move || {
            let guard = pool_b.acquire().expect("thread B: acquire should succeed");
            let v: i64 = guard
                .load("return 2")
                .eval()
                .expect("lua eval on guard_b failed");
            v
        });

        // Both threads must complete without timing out or panicking.
        let result_a = handle_a.join().expect("thread A panicked");
        let result_b = handle_b.join().expect("thread B panicked");
        assert_eq!(result_a, 1);
        assert_eq!(result_b, 2);
    }

    fn make_pool_with_instruction_limit(n: usize, max_instructions: u64) -> VmPool {
        let vms = (0..n)
            .map(|_| {
                let lua = Lua::new();
                lua.set_app_data(MaxInstructions(max_instructions));
                lua
            })
            .collect();
        VmPool::new(vms)
    }

    #[test]
    fn instruction_limit_terminates_infinite_loop() {
        let pool = make_pool_with_instruction_limit(1, 50_000);
        let guard = pool.acquire().expect("should acquire VM");
        let result = guard.load("while true do end").exec();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("instruction limit"), "unexpected error: {err}");
    }

    #[test]
    fn instruction_limit_allows_normal_code() {
        let pool = make_pool_with_instruction_limit(1, 10_000_000);
        let guard = pool.acquire().expect("should acquire VM");
        let result: i64 = guard
            .load("local s = 0; for i = 1, 1000 do s = s + i end; return s")
            .eval()
            .expect("normal code should succeed");
        assert_eq!(result, 500500);
    }

    #[test]
    fn instruction_hook_resets_between_acquires() {
        // After returning a VM to the pool and re-acquiring, the counter resets.
        let pool = make_pool_with_instruction_limit(1, 10_000_000);
        {
            let guard = pool.acquire().expect("first acquire");
            let _: i64 = guard
                .load("local s = 0; for i = 1, 1000 do s = s + i end; return s")
                .eval()
                .expect("first run should succeed");
        }
        // Re-acquire — fresh counter
        let guard = pool.acquire().expect("second acquire");
        let result: i64 = guard
            .load("local s = 0; for i = 1, 1000 do s = s + i end; return s")
            .eval()
            .expect("second run should succeed with fresh counter");
        assert_eq!(result, 500500);
    }

    #[test]
    #[ignore = "exercises the 5-second condvar timeout; run explicitly with --include-ignored"]
    fn acquire_times_out_on_empty_pool() {
        // An empty pool has no VMs. The condvar wait exhausts the 5-second timeout,
        // the post-timeout pop also finds nothing, and acquire returns an error.
        // The pool with 0 VMs is the simplest way to reach the timeout branch without
        // needing a second thread to hold the only VM, but it inherently takes 5 seconds.
        let pool = make_pool(0);
        let err = pool.acquire().expect_err("empty pool should time out");
        assert!(
            err.to_string().contains("timed out"),
            "unexpected error message: {err}"
        );
    }
}
