//! The process-wide JSON data-nesting limit.
//!
//! Lives in `core` because both the config layer (which installs it) and the
//! data converters (which enforce it) depend on `core`, and neither may depend
//! on the other.

use std::sync::OnceLock;

/// Fallback nesting limit used before config is applied (and in unit tests /
/// CLI paths that never install one). Matches the `depth.max_nesting_depth`
/// config default.
const DEFAULT_MAX_NESTING_DEPTH: usize = 64;

/// A write-once nesting limit. The process has exactly one
/// ([`NESTING_DEPTH`]); the type exists so the config's install step can be
/// exercised against a fresh store in a test.
pub struct NestingDepth(OnceLock<usize>);

impl NestingDepth {
    /// An empty store that answers the default until a limit is installed.
    #[must_use]
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    /// Install the limit. Subsequent installs are ignored — the limit is
    /// fixed for the store's lifetime.
    pub fn install(&self, limit: usize) {
        let _ = self.0.set(limit);
    }

    /// The installed limit, or the default before one is installed.
    #[must_use]
    pub fn get(&self) -> usize {
        self.0.get().copied().unwrap_or(DEFAULT_MAX_NESTING_DEPTH)
    }
}

impl Default for NestingDepth {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide store, installed from `depth.max_nesting_depth` when the
/// config is applied. Distinct from relationship population depth.
pub static NESTING_DEPTH: NestingDepth = NestingDepth::new();

/// Install the configured data-nesting limit process-wide (called once when
/// the config is applied).
pub fn set_max_nesting_depth(limit: usize) {
    NESTING_DEPTH.install(limit);
}

/// The process-wide JSON data-nesting limit (`depth.max_nesting_depth`). Shared
/// by every data-ingestion converter so the Lua↔JSON path and the gRPC↔JSON
/// path reject over-deep data identically, guarding against stack overflow.
#[must_use]
pub fn max_nesting_depth() -> usize {
    NESTING_DEPTH.get()
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_MAX_NESTING_DEPTH, NestingDepth};

    #[test]
    fn a_fresh_store_answers_the_default_until_installed() {
        let store = NestingDepth::new();
        assert_eq!(store.get(), DEFAULT_MAX_NESTING_DEPTH);

        store.install(7);
        assert_eq!(store.get(), 7);

        store.install(9);
        assert_eq!(store.get(), 7, "the first install wins");
    }
}
