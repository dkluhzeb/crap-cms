//! Self-update check configuration.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateConfig {
    /// On `crap-cms serve` startup, print a one-line notice if the cached
    /// update-check shows a newer release is available. Cache is populated by
    /// `crap-cms update check` (24h TTL); startup never performs a network
    /// request. Default: true.
    pub check_on_startup: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            check_on_startup: true,
        }
    }
}
