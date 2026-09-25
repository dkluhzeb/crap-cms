//! Lua VM pool for concurrent hook execution.
//!
//! The pool is **elastic**: it pre-warms `vm_pool_size` VMs and then grows on
//! demand up to `max_vm_pool_size` as concurrency rises, reusing returned VMs
//! across threads. Only when every VM up to the cap is checked out does a
//! further `acquire` briefly wait for one to come back. This replaces the old
//! fixed-size pool, which blocked up to 5s whenever concurrency exceeded the
//! pool size regardless of available capacity.

use anyhow::{Context as _, Result, anyhow};
use mlua::{Error::RuntimeError, HookTriggers, Lua, Result as LuaResult, VmState};
use std::{
    fmt,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::config::HooksConfig;
use crate::core::lua_lease::LuaVmLease;
use crate::hooks::lifecycle::types::{
    ExecutionDeadline, ExecutionDeadlineGuard, InstructionCounter, MaxInstructions,
    check_execution_deadline,
};

/// Builds a fresh, fully-initialized pool VM. The `usize` is the VM index
/// (used only for the `vm-N` label). Boxed so the pool is decoupled from the
/// concrete construction (production wires in `create_lua_vm`; tests inject a
/// trivial factory).
pub(super) type VmFactory = Box<dyn Fn(usize) -> Result<Lua> + Send + Sync>;

/// How long `acquire` waits for a returned VM once the pool is at its cap and
/// every VM is checked out.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Every VM up to the cap was checked out for the whole wait.
///
/// A TYPED error, not a message: the condition is structurally the DB pool's
/// checkout timeout — the same request retried a moment later succeeds — and
/// the surfaces classify it by downcast, so it reports as retryable (503 /
/// `UNAVAILABLE`) instead of an internal fault. Matching on the text would
/// put the verdict back in a second place.
#[derive(Debug, Clone, Copy)]
pub struct VmPoolExhausted {
    /// Seconds `acquire` waited before giving up.
    pub waited_secs: u64,
    /// The pool's hard ceiling on live VMs — every one of them was busy.
    pub cap: usize,
}

impl fmt::Display for VmPoolExhausted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "VM pool acquire timed out after {}s (all {} VMs busy)",
            self.waited_secs, self.cap
        )
    }
}

impl std::error::Error for VmPoolExhausted {}

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

    /// [`with_vm`](Self::with_vm) under an [`ExecutionDeadline`] of
    /// `timeout_secs`, with the VM hook armed so even a CPU-bound callback
    /// stops at it — the same bound a job handler's lease carries.
    fn with_vm_until(
        &self,
        timeout_secs: u64,
        f: &mut dyn FnMut(&Lua) -> Result<()>,
    ) -> Result<()> {
        let guard = self.acquire()?;
        let _deadline =
            ExecutionDeadlineGuard::install(&guard, ExecutionDeadline::new(timeout_secs));
        let _hook = DeadlineHookGuard::arm(&guard)
            .map_err(|e| anyhow!("failed to arm the Lua VM hook: {e}"))?;

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
                return self.check_out(vm);
            }

            // Room to grow: reserve a slot, build outside the lock.
            if inner.live < self.cap {
                inner.live += 1;
                drop(inner);

                match (self.factory)(self.next_index.fetch_add(1, Ordering::Relaxed)) {
                    Ok(vm) => return self.check_out(vm),
                    Err(e) => {
                        self.release_slot();

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
                return Err(anyhow::Error::new(VmPoolExhausted {
                    waited_secs: ACQUIRE_TIMEOUT.as_secs(),
                    cap: self.cap,
                }));
            }
        }
    }

    /// Arm the instruction budget (when one is configured) and wrap the VM in
    /// a returning guard.
    ///
    /// A VM whose hook cannot be installed is dropped rather than leased:
    /// leasing it would hand out a VM with no ceiling on how long a hook may
    /// run, which is the one thing the budget exists to prevent.
    fn check_out(&self, vm: Lua) -> Result<VmGuard<'_>> {
        if instruction_budget(&vm) > 0
            && let Err(e) = set_vm_hook(&vm)
        {
            self.release_slot();

            return Err(anyhow!("failed to arm the Lua VM hook: {e}"));
        }

        Ok(VmGuard {
            pool: self,
            vm: Some(vm),
        })
    }

    /// Give back the `live` slot of a VM that never became a lease, and wake a
    /// waiter so the freed capacity is usable again.
    fn release_slot(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.live -= 1;
        }

        self.available.notify_one();
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
                vm.remove_global_hook();
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

/// Re-arm the instruction budget on a leased VM.
///
/// The counter is armed once per lease. A Rust-driven loop that runs one hook
/// per document on a single lease (batched `after_read`, batched field-access
/// strip) would otherwise spend the whole budget across the page: past the
/// cap every later document's hook fails — silently, for fail-open
/// `after_read` — so the transform would vanish from the tail of a large
/// page. Calling this per document makes the budget per hook invocation, as
/// it is for single-document calls. Not reachable from Lua (a hook cannot
/// extend its own budget).
pub(crate) fn reset_instruction_budget(vm: &Lua) {
    if let Some(counter) = vm.app_data_ref::<InstructionCounter>() {
        counter.0.store(0, Ordering::Relaxed);
    }
}

/// Apply the configured `[hooks]` resource limits to a fresh VM: the memory
/// ceiling (`max_memory`) and the instruction budget (`max_instructions`),
/// armed right away so the VM's own setup — definition files, `init.lua`,
/// required modules — runs under it too. Shared by the init VM and every
/// pool VM, so a runaway `init.lua` fails the boot with the limit's error
/// instead of hanging it. Setup code re-arms the budget per file
/// ([`reset_instruction_budget`]); a lease re-arms it at check-out.
///
/// # Errors
///
/// Returns an error if the memory limit or the VM hook cannot be installed.
pub(crate) fn apply_vm_limits(lua: &Lua, hooks: &HooksConfig) -> Result<()> {
    if hooks.max_memory > 0 {
        // 32-bit overflow path falls back to 256 MiB (a sane VM memory ceiling)
        // rather than usize::MAX, which would effectively disable the limit.
        let memory_limit = usize::try_from(hooks.max_memory).unwrap_or(256 * 1024 * 1024);
        lua.set_memory_limit(memory_limit)?;
    }

    lua.set_app_data(MaxInstructions(hooks.max_instructions));

    if hooks.max_instructions > 0 {
        set_vm_hook(lua).context("failed to arm the Lua instruction budget")?;
    }

    Ok(())
}

/// The lease's instruction budget; `0` means none is configured.
fn instruction_budget(vm: &Lua) -> u64 {
    vm.app_data_ref::<MaxInstructions>().map_or(0, |m| m.0)
}

/// Install the VM's global hook: the instruction budget (when
/// `MaxInstructions` is configured) and the job deadline (when one is
/// installed — see `ExecutionDeadline`).
///
/// Armed only when something needs it — at check-out when a budget is
/// configured, and by [`DeadlineHookGuard`] for a job handler's lease — so a
/// VM with neither pays no per-instruction hook overhead.
///
/// The hook is the VM's **global** hook, not a per-thread one. A per-thread
/// hook is looked up by thread on every trigger and uninstalls itself on a
/// miss — and a coroutine is a new thread that inherits the parent's hook
/// pointer, so `coroutine.wrap(function() while true do end end)()` inside
/// a hook would run with no ceiling, never return the lease, and starve the
/// pool. The global hook reads its callback from VM-wide state, so every
/// thread — main or coroutine — counts against the one shared budget.
///
/// The install error is propagated: a swallowed one leaves the VM running
/// without the ceiling, and nothing downstream would notice.
fn set_vm_hook(vm: &Lua) -> LuaResult<()> {
    let max = instruction_budget(vm);
    let counter = Arc::new(AtomicU64::new(0));

    if max > 0 {
        vm.set_app_data(InstructionCounter(counter.clone()));
    }

    vm.set_global_hook(
        HookTriggers::new().every_nth_instruction(HOOK_EVERY_NTH_INSTRUCTION),
        move |lua, _debug| {
            check_execution_deadline(lua)?;

            check_instruction_budget(&counter, max)?;

            Ok(VmState::Continue)
        },
    )
}

/// Keeps the VM hook armed for a job handler's deadline while held.
///
/// A job runs its handler under an `ExecutionDeadline`, and a CPU-bound
/// handler that never reaches a database, HTTP or email call can only notice
/// the deadline from the VM hook. A lease with an instruction budget already
/// carries the hook (which checks the deadline too); on one without, this
/// arms it — and disarms it again on drop, so the VM goes back to running
/// hook-free.
pub(crate) struct DeadlineHookGuard<'a> {
    vm: &'a Lua,
    /// Whether this guard installed the hook (and so removes it on drop).
    armed_here: bool,
}

impl<'a> DeadlineHookGuard<'a> {
    /// Arm the hook on `vm` unless its instruction budget already did.
    ///
    /// # Errors
    ///
    /// The hook install error — the handler must not run without a way to
    /// stop at its deadline.
    pub(crate) fn arm(vm: &'a Lua) -> LuaResult<Self> {
        let armed_here = instruction_budget(vm) == 0;

        if armed_here {
            set_vm_hook(vm)?;
        }

        Ok(Self { vm, armed_here })
    }
}

impl Drop for DeadlineHookGuard<'_> {
    fn drop(&mut self) {
        if self.armed_here {
            self.vm.remove_global_hook();
        }
    }
}

/// How many VM instructions pass between two hook invocations.
const HOOK_EVERY_NTH_INSTRUCTION: u32 = 10_000;

/// Charge one hook interval against the lease's budget; `max == 0` means no
/// budget is configured.
fn check_instruction_budget(counter: &AtomicU64, max: u64) -> LuaResult<()> {
    if max == 0 {
        return Ok(());
    }

    let step = u64::from(HOOK_EVERY_NTH_INSTRUCTION);
    let count = counter.fetch_add(step, Ordering::Relaxed);

    if count + step > max {
        return Err(RuntimeError(
            "Lua execution exceeded instruction limit".into(),
        ));
    }

    Ok(())
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
    use anyhow::bail;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    use super::*;

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

    /// An unbounded loop must hit the ceiling — i.e. the VM was leased armed.
    fn assert_budget_armed(vm: &Lua, which: &str) {
        let err = vm
            .load("while true do end")
            .exec()
            .expect_err("an unbounded loop must hit the ceiling");

        assert!(
            err.to_string().contains("instruction limit"),
            "the {which} lease was handed out without an instruction ceiling: {err}"
        );
    }

    /// Arming the budget is part of checking a VM out, not a best-effort
    /// extra: the install error used to be discarded, which would have handed
    /// out VMs with no ceiling at all. Every path that produces a lease — the
    /// pre-warmed VM, one grown on demand, and a reused one — comes back armed.
    #[test]
    fn every_lease_comes_back_with_the_budget_armed() {
        let pool = make_pool_with_instruction_limit(2, 50_000);

        let prewarmed = pool.acquire().expect("pre-warmed lease");
        let grown = pool.acquire().expect("on-demand lease");

        assert_budget_armed(&prewarmed, "pre-warmed");
        assert_budget_armed(&grown, "on-demand");

        drop((prewarmed, grown));

        let reused = pool.acquire().expect("reused lease");
        assert_budget_armed(&reused, "reused");
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

    /// A coroutine is a new Lua thread. The budget must follow it: a loop
    /// spinning inside `coroutine.wrap` hits the ceiling instead of running
    /// forever with the lease never returned.
    #[test]
    fn instruction_limit_applies_inside_a_wrapped_coroutine() {
        let pool = make_pool_with_instruction_limit(1, 50_000);
        let guard = pool.acquire().expect("should acquire VM");

        let err = guard
            .load("coroutine.wrap(function() while true do end end)()")
            .exec()
            .expect_err("a coroutine must not escape the instruction limit");

        assert!(
            err.to_string().contains("instruction limit"),
            "unexpected error: {err}"
        );
    }

    /// Same ceiling for `coroutine.create` + `coroutine.resume`: the resume
    /// reports the limit error instead of never returning.
    #[test]
    fn instruction_limit_applies_inside_a_resumed_coroutine() {
        let pool = make_pool_with_instruction_limit(1, 50_000);
        let guard = pool.acquire().expect("should acquire VM");

        let err = guard
            .load(
                r#"
                local co = coroutine.create(function() while true do end end)
                local ok, err = coroutine.resume(co)
                assert(not ok, "the coroutine ran to completion without a ceiling")
                error(err, 0)
                "#,
            )
            .exec()
            .expect_err("a resumed coroutine must not escape the instruction limit");

        assert!(
            err.to_string().contains("instruction limit"),
            "unexpected error: {err}"
        );
    }

    /// The budget is shared across threads: work done inside a coroutine
    /// counts against the same lease budget as the main thread.
    #[test]
    fn coroutine_work_counts_against_the_shared_budget() {
        let pool = make_pool_with_instruction_limit(1, 200_000);
        let guard = pool.acquire().expect("should acquire VM");
        let burn_in_coroutine = r"
            coroutine.wrap(function()
                local x = 0
                for i = 1, 20000 do x = x + 1 end
            end)()
        ";

        let mut fitted = 0;
        while guard.load(burn_in_coroutine).exec().is_ok() {
            fitted += 1;
            assert!(
                fitted < 100,
                "coroutine work never tripped the shared budget"
            );
        }
        assert!(fitted >= 1, "a single coroutine burst must fit on its own");
    }

    /// The budget is per lease; a Rust-driven batch loop re-arms it per
    /// document so the tail of a large page doesn't run out.
    #[test]
    fn instruction_budget_can_be_re_armed_within_a_lease() {
        let pool = make_pool_with_instruction_limit(1, 200_000);
        let guard = pool.acquire().expect("should acquire VM");
        let burn = "local x = 0 for i = 1, 20000 do x = x + 1 end";

        // The budget is shared across calls on one lease: repeating a call
        // that fits on its own eventually trips the cap.
        let mut fitted = 0;
        while guard.load(burn).exec().is_ok() {
            fitted += 1;
            assert!(fitted < 100, "the shared budget never tripped");
        }
        assert!(fitted >= 1, "a single call must fit the budget on its own");

        reset_instruction_budget(&guard);
        guard
            .load(burn)
            .exec()
            .expect("after re-arming, the same call fits again");
    }

    /// A loop long enough to cross the hook interval many times.
    const LONG_LOOP: &str = "local s = 0; for i = 1, 1000000 do s = s + i end; return s";

    /// Regression: the VM hook was armed on every lease, so with no
    /// instruction budget configured every hook still paid the per-interval
    /// hook overhead. A lease with no budget now runs hook-free: even an
    /// expired deadline (which the hook would enforce) goes unnoticed by a
    /// pure loop until a [`DeadlineHookGuard`] arms the hook — and once the
    /// guard is gone the VM is hook-free again.
    #[test]
    fn a_lease_without_a_budget_runs_hook_free_until_a_deadline_arms_it() {
        let pool = make_pool(1, 1);
        let guard = pool.acquire().expect("should acquire VM");
        let _deadline = ExecutionDeadlineGuard::install(&guard, ExecutionDeadline::new(0));

        guard
            .load(LONG_LOOP)
            .exec()
            .expect("no hook is armed without a budget or a deadline hook");

        {
            let _hook = DeadlineHookGuard::arm(&guard).expect("arm the deadline hook");

            let err = guard
                .load(LONG_LOOP)
                .exec()
                .expect_err("the armed hook enforces the deadline");
            assert!(
                err.to_string().contains("exceeded its timeout"),
                "unexpected error: {err}"
            );
        }

        guard
            .load(LONG_LOOP)
            .exec()
            .expect("dropping the guard disarms the hook");
    }

    /// With a budget configured the lease is already armed: the deadline
    /// guard leaves that hook alone, before and after.
    #[test]
    fn a_deadline_hook_keeps_the_budget_hook_of_a_budgeted_lease() {
        let pool = make_pool_with_instruction_limit(1, 50_000);
        let guard = pool.acquire().expect("should acquire VM");

        drop(DeadlineHookGuard::arm(&guard).expect("arm the deadline hook"));

        assert_budget_armed(&guard, "budgeted");
    }

    /// Regression: a job handler that never reaches a database or HTTP call
    /// had no way to notice its timeout — with no instruction budget
    /// configured the VM carried no hook at all, so a CPU loop ran on past the
    /// deadline while the scheduler already retried the run. The deadline
    /// hook stops the loop once the deadline has passed.
    #[test]
    fn a_cpu_loop_stops_at_the_job_deadline_without_an_instruction_budget() {
        let pool = make_pool(1, 1);
        let guard = pool.acquire().expect("should acquire VM");
        let _deadline = ExecutionDeadlineGuard::install(&guard, ExecutionDeadline::new(0));
        let _hook = DeadlineHookGuard::arm(&guard).expect("arm the deadline hook");

        let err = guard
            .load("while true do end")
            .exec()
            .expect_err("the loop must stop at the deadline");

        assert!(
            err.to_string().contains("exceeded its timeout"),
            "unexpected error: {err}"
        );
    }

    /// The deadline follows a coroutine like the budget does.
    #[test]
    fn the_job_deadline_applies_inside_a_coroutine() {
        let pool = make_pool(1, 1);
        let guard = pool.acquire().expect("should acquire VM");
        let _deadline = ExecutionDeadlineGuard::install(&guard, ExecutionDeadline::new(0));
        let _hook = DeadlineHookGuard::arm(&guard).expect("arm the deadline hook");

        let err = guard
            .load("coroutine.wrap(function() while true do end end)()")
            .exec()
            .expect_err("a coroutine must not escape the deadline");

        assert!(
            err.to_string().contains("exceeded its timeout"),
            "unexpected error: {err}"
        );
    }

    /// A deadline still ahead leaves ordinary code alone, with a budget
    /// configured or not.
    #[test]
    fn a_future_job_deadline_leaves_normal_code_alone() {
        for pool in [
            make_pool(1, 1),
            make_pool_with_instruction_limit(1, 10_000_000),
        ] {
            let guard = pool.acquire().expect("should acquire VM");
            let _deadline = ExecutionDeadlineGuard::install(&guard, ExecutionDeadline::new(3600));
            let _hook = DeadlineHookGuard::arm(&guard).expect("arm the deadline hook");

            let result: i64 = guard
                .load("local s = 0; for i = 1, 100000 do s = s + i end; return s")
                .eval()
                .expect("normal code should succeed");

            assert_eq!(result, 5_000_050_000);
        }
    }

    /// Regression: a scheduler-run custom email provider ran on a pooled VM
    /// with no deadline, so a hung `send` held the email queue slot (and the
    /// MFA / reset mails behind it) far past the queue's timeout. The
    /// deadline-bounded lease stops it — and leaves the VM deadline-free for
    /// the next lease.
    #[test]
    fn a_deadline_bounded_lease_stops_its_callback() {
        let pool = make_pool(1, 1);

        let err = pool
            .with_vm_until(0, &mut |lua| {
                lua.load("while true do end").exec()?;
                Ok(())
            })
            .expect_err("the callback must stop at the deadline");
        assert!(err.to_string().contains("exceeded its timeout"), "{err}");

        pool.with_vm(&mut |lua| {
            assert!(
                check_execution_deadline(lua).is_ok(),
                "the deadline must not leak"
            );
            lua.load(LONG_LOOP).exec()?;
            Ok(())
        })
        .expect("the next lease runs unbounded");
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

    /// A pool exhaustion carries its TYPED cause through the `anyhow` context
    /// layers a caller adds, so the classifier downcasts instead of matching
    /// the wording — and the wording itself is unchanged for the log.
    #[test]
    fn exhaustion_is_a_typed_cause_that_survives_context() {
        let e = Err::<(), _>(anyhow::Error::new(VmPoolExhausted {
            waited_secs: 5,
            cap: 8,
        }))
        .context("Failed to run before_change hooks")
        .unwrap_err();

        let typed = e
            .downcast_ref::<VmPoolExhausted>()
            .expect("the typed cause must survive context layers");
        assert_eq!(typed.cap, 8);
        assert_eq!(typed.waited_secs, 5);
        assert!(
            format!("{e:#}").contains("all 8 VMs busy"),
            "the log wording is preserved: {e:#}"
        );
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
