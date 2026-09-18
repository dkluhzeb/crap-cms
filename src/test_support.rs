//! Crate-wide helpers for the unit tests, compiled only under `cfg(test)`.
//!
//! Everything here guards state that is shared by the whole test *process*,
//! which no single module can serialize on its own.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Serializes every unit test that touches the process environment.
///
/// `set_var` / `remove_var` mutate state every thread in the process reads,
/// and the test harness runs tests on many threads at once, so a concurrent
/// set and read is a data race. One lock covers the whole binary: two modules
/// each holding a lock of their own would still race against each other.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Hold the environment lock for the remainder of the current test.
///
/// A test that panics while holding the lock poisons the mutex; the guard is
/// taken from the poisoned mutex anyway, so one failing test does not turn
/// every other environment test into a failure as well.
pub(crate) fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}
