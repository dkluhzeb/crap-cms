//! Lua VM pool for concurrent hook execution.
//!
//! The pool is **elastic**: it pre-warms `vm_pool_size` VMs and then grows on
//! demand up to `max_vm_pool_size` as concurrency rises, reusing returned VMs
//! across threads. Only when every VM up to the cap is checked out does a
//! further `acquire` briefly wait for one to come back. This replaces the old
//! fixed-size pool, which blocked up to 5s whenever concurrency exceeded the
//! pool size regardless of available capacity.

use anyhow::{Context as _, Result, anyhow, bail};
use mlua::{HookTriggers, Lua, VmState};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::core::lua_lease::LuaVmLease;
use crate::hooks::lifecycle::types::MaxInstructions;

/// Builds a fresh, fully-initialized pool VM. The `usize` is the VM index
/// (used only for the `vm-N` label). Boxed so the pool is decoupled from the
/// concrete construction (production wires in `create_lua_vm`; tests inject a
/// trivial factory).
pub(super) type VmFactory = Box<dyn Fn(usize) -> Result<Lua> + Send + Sync>;

/// How long `acquire` waits for a returned VM once the pool is at its cap and
/// every VM is checked out.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

struct PoolInner {
    /// VMs available for immediate reuse.
    idle: Vec<Lua>,
    /// Total VMs created (idle + checked-out). Never exceeds `cap`.
    live: usize,
}

/// Elastic pool of Lua VMs for concurrent hook execution.
pub(super) struct VmPool {
    inner: Mutex<PoolInner>,
    available: Condvar,
    factory: VmFactory,
    /// Hard ceiling on `live`.
    cap: usize,
    /// Monotonic VM index for labels; continues past the pre-warmed VMs.
    next_index: AtomicUsize,
}

impl LuaVmLease for VmPool {
    /// Check a VM out of the pool for the duration of `f`. Gives external
    /// callers (scheduler, HTTP handlers) real concurrency on custom Lua
    /// providers. Never call from inside a pool VM — that would re-enter
    /// the pool and can deadlock; use a `LocalLease` there instead.
    fn with_vm(&self, f: &mut dyn FnMut(&Lua) -> Result<()>) -> Result<()> {
        let guard = self.acquire()?;
        f(&guard)
    }
}

impl VmPool {
    /// Create the pool from its pre-warmed VMs plus the factory for on-demand
    /// growth. `cap` is floored at the pre-warm count so the pre-warmed VMs
    /// always fit.
    pub(super) fn new(prewarmed: Vec<Lua>, factory: VmFactory, cap: usize) -> Self {
        let live = prewarmed.len();
        VmPool {
            inner: Mutex::new(PoolInner {
                idle: prewarmed,
                live,
            }),
            available: Condvar::new(),
            factory,
            cap: cap.max(live),
            next_index: AtomicUsize::new(live + 1),
        }
    }

    /// Acquire a VM: reuse an idle one, else build a new one while under the
    /// cap, else wait (up to [`ACQUIRE_TIMEOUT`]) for one to be returned.
    pub(super) fn acquire(&self) -> Result<VmGuard<'_>> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|e| anyhow!("VM pool lock poisoned: {e}"))?;

        loop {
            if let Some(vm) = inner.idle.pop() {
                drop(inner);
                return Ok(self.check_out(vm));
            }

            // Room to grow: reserve a slot, build outside the lock.
            if inner.live < self.cap {
                inner.live += 1;
                drop(inner);

                match (self.factory)(self.next_index.fetch_add(1, Ordering::Relaxed)) {
                    Ok(vm) => return Ok(self.check_out(vm)),
                    Err(e) => {
                        // Roll the reservation back and wake a waiter to retry.
                        if let Ok(mut inner) = self.inner.lock() {
                            inner.live -= 1;
                        }
                        self.available.notify_one();
                        return Err(e).context("failed to build a pool Lua VM");
                    }
                }
            }

            // At cap and none idle — wait for a returned VM.
            let (guard, wait) = self
                .available
                .wait_timeout(inner, ACQUIRE_TIMEOUT)
                .map_err(|e| anyhow!("VM pool condvar wait failed: {e}"))?;
            inner = guard;

            if wait.timed_out() && inner.idle.is_empty() && inner.live >= self.cap {
                bail!(
                    "VM pool acquire timed out after {}s (all {} VMs busy)",
                    ACQUIRE_TIMEOUT.as_secs(),
                    self.cap
                );
            }
        }
    }

    /// Arm the instruction hook and wrap the VM in a returning guard.
    fn check_out(&self, vm: Lua) -> VmGuard<'_> {
        set_instruction_hook(&vm);
        VmGuard {
            pool: self,
            vm: Some(vm),
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
        let Some(vm) = self.vm.take() else { return };

        match self.pool.inner.lock() {
            Ok(mut inner) => {
                vm.remove_hook();
                inner.idle.push(vm);
                self.pool.available.notify_one();
            }
            // A poisoned lock means a thread panicked while holding it. The
            // pool is effectively dead (every `acquire` will also fail on the
            // poisoned lock), so we can only drop the VM. Log it rather than
            // fail silently.
            Err(_) => {
                tracing::error!("VM pool mutex poisoned; dropping a Lua VM");
            }
        }
    }
}

/// Set an instruction-counting hook on the VM if `MaxInstructions` is configured.
fn set_instruction_hook(vm: &Lua) {
    let max = vm.app_data_ref::<MaxInstructions>().map_or(0, |m| m.0);
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
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    /// A pool whose factory builds bare VMs and counts how many it built.
    fn make_pool_counting(prewarm: usize, cap: usize) -> (Arc<VmPool>, Arc<AtomicUsize>) {
        let built = Arc::new(AtomicUsize::new(0));
        let b = Arc::clone(&built);
        let factory: VmFactory = Box::new(move |_idx| {
            b.fetch_add(1, Ordering::Relaxed);
            Ok(Lua::new())
        });
        let prewarmed = (0..prewarm)
            .map(|_| {
                built.fetch_add(1, Ordering::Relaxed);
                Lua::new()
            })
            .collect();
        (Arc::new(VmPool::new(prewarmed, factory, cap)), built)
    }

    fn make_pool(prewarm: usize, cap: usize) -> VmPool {
        let factory: VmFactory = Box::new(|_idx| Ok(Lua::new()));
        let prewarmed = (0..prewarm).map(|_| Lua::new()).collect();
        VmPool::new(prewarmed, factory, cap)
    }

    #[test]
    fn acquire_returns_valid_vm() {
        let pool = make_pool(1, 4);
        let guard = pool.acquire().expect("should acquire VM");
        let result: i64 = guard.load("return 1 + 1").eval().expect("lua eval failed");
        assert_eq!(result, 2);
    }

    #[test]
    fn drop_returns_vm_to_pool() {
        let pool = make_pool(1, 4);
        {
            let _guard = pool.acquire().expect("first acquire should succeed");
        }
        let guard2 = pool.acquire().expect("acquire after drop should succeed");
        let result: i64 = guard2.load("return 42").eval().expect("lua eval failed");
        assert_eq!(result, 42);
    }

    #[test]
    fn grows_beyond_prewarm_up_to_cap() {
        // Pre-warm 1, cap 3. Holding all three simultaneously forces two
        // on-demand builds; a fourth concurrent acquire would exceed the cap.
        let (pool, built) = make_pool_counting(1, 3);
        let g1 = pool.acquire().expect("acquire 1");
        let g2 = pool.acquire().expect("acquire 2 (built on demand)");
        let g3 = pool.acquire().expect("acquire 3 (built on demand)");
        assert_eq!(built.load(Ordering::Relaxed), 3, "pool grew to the cap");
        drop((g1, g2, g3));

        // Reusing returned VMs must not build more.
        let _g = pool.acquire().expect("reuse");
        assert_eq!(built.load(Ordering::Relaxed), 3, "reuse builds nothing new");
    }

    #[test]
    fn at_cap_blocks_then_serves_a_returned_vm() {
        // Cap 1: the second acquire must wait until the first is returned.
        let pool = Arc::new({
            let factory: VmFactory = Box::new(|_idx| Ok(Lua::new()));
            VmPool::new(vec![Lua::new()], factory, 1)
        });

        let g1 = pool.acquire().expect("acquire the only VM");

        let p2 = Arc::clone(&pool);
        let handle = thread::spawn(move || {
            // Blocks until g1 is dropped on the main thread, then succeeds.
            let g = p2
                .acquire()
                .expect("second acquire should succeed after return");
            let v: i64 = g.load("return 7").eval().expect("eval");
            v
        });

        // Give the spawned thread time to reach the wait, then return the VM.
        thread::sleep(Duration::from_millis(100));
        drop(g1);

        assert_eq!(handle.join().expect("thread panicked"), 7);
    }

    #[test]
    fn concurrent_acquire_grows() {
        let pool = Arc::new(make_pool(0, 2));
        let a = Arc::clone(&pool);
        let b = Arc::clone(&pool);
        let ha = thread::spawn(move || {
            let g = a.acquire().expect("thread A acquire");
            g.load("return 1").eval::<i64>().expect("eval A")
        });
        let hb = thread::spawn(move || {
            let g = b.acquire().expect("thread B acquire");
            g.load("return 2").eval::<i64>().expect("eval B")
        });
        assert_eq!(ha.join().unwrap(), 1);
        assert_eq!(hb.join().unwrap(), 2);
    }

    fn make_pool_with_instruction_limit(cap: usize, max_instructions: u64) -> VmPool {
        let factory: VmFactory = Box::new(move |_idx| {
            let lua = Lua::new();
            lua.set_app_data(MaxInstructions(max_instructions));
            Ok(lua)
        });
        let seed = Lua::new();
        seed.set_app_data(MaxInstructions(max_instructions));
        VmPool::new(vec![seed], factory, cap)
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
        let pool = make_pool_with_instruction_limit(1, 10_000_000);
        {
            let guard = pool.acquire().expect("first acquire");
            let _: i64 = guard
                .load("local s = 0; for i = 1, 1000 do s = s + i end; return s")
                .eval()
                .expect("first run should succeed");
        }
        let guard = pool.acquire().expect("second acquire");
        let result: i64 = guard
            .load("local s = 0; for i = 1, 1000 do s = s + i end; return s")
            .eval()
            .expect("second run should succeed with fresh counter");
        assert_eq!(result, 500500);
    }

    #[test]
    fn build_failure_frees_the_reserved_slot() {
        // A factory that always fails: acquire errors, but the reserved slot
        // is rolled back so `live` never leaks (a later successful factory
        // could still grow). Here we just assert the error surfaces and a
        // second attempt also errors (not a spurious cap timeout).
        let factory: VmFactory = Box::new(|_idx| bail!("boom"));
        let pool = VmPool::new(vec![], factory, 2);
        let e1 = pool.acquire().unwrap_err();
        assert!(e1.to_string().contains("build a pool Lua VM"), "{e1}");
        let e2 = pool.acquire().unwrap_err();
        assert!(e2.to_string().contains("build a pool Lua VM"), "{e2}");
    }
}
