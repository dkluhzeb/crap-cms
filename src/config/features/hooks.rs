//! Lua hook configuration -- `on_init` scripts, recursion limits, VM resources.

use std::thread;

use serde::{Deserialize, Serialize};

use crate::config::parsing::serde_filesize;

/// Hook configuration -- `on_init` script references and recursion limits.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HooksConfig {
    /// List of Lua script names (without extension) to run once when the CMS starts up.
    /// These are loaded from the `hooks/` directory.
    pub on_init: Vec<String>,
    /// Max hook recursion depth for Lua CRUD -> hook -> CRUD chains.
    /// 0 = disable hooks from Lua CRUD entirely. Default: 3.
    pub max_depth: u32,
    /// Number of Lua VMs in the hook runner pool. Default: the number of
    /// available CPU cores (`available_parallelism`), or 4 if that can't be
    /// detected; the effective value is floored at 1. Higher values allow
    /// more concurrent hook execution — raise it when using a custom
    /// (`crap.storage`/`crap.email`) provider, whose ops hold a VM for the
    /// duration of a network round-trip.
    pub vm_pool_size: usize,
    /// Maximum Lua instructions per hook invocation. 0 = unlimited. Default: `10_000_000`.
    pub max_instructions: u64,
    /// Maximum Lua memory in bytes per VM. 0 = unlimited. Default: `52_428_800` (50 MB).
    /// Accepts integer bytes or human-readable string ("50MB", "100MB").
    #[serde(with = "serde_filesize")]
    pub max_memory: u64,
    /// Allow Lua HTTP requests to private/internal networks. Default: false.
    pub allow_private_networks: bool,
    /// Maximum HTTP response body size in bytes for `crap.http.request`. Default: `10_485_760` (10 MB).
    /// Increase if hooks need to download large files (e.g. video processing).
    /// Accepts integer bytes or human-readable string ("10MB", "1GB").
    #[serde(with = "serde_filesize")]
    pub http_max_response_bytes: u64,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            on_init: Vec::new(),
            max_depth: 3,
            vm_pool_size: thread::available_parallelism().map_or(4, std::num::NonZero::get),
            max_instructions: 10_000_000,
            max_memory: 52_428_800, // 50 MB
            allow_private_networks: false,
            http_max_response_bytes: 10 * 1024 * 1024, // 10 MB
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_config_defaults() {
        let hooks = HooksConfig::default();
        assert!(hooks.on_init.is_empty());
        assert_eq!(hooks.max_depth, 3);
        assert!(hooks.vm_pool_size >= 1);
        assert_eq!(hooks.max_instructions, 10_000_000);
        assert_eq!(hooks.max_memory, 52_428_800);
        assert!(!hooks.allow_private_networks);
        assert_eq!(hooks.http_max_response_bytes, 10 * 1024 * 1024);
    }
}
