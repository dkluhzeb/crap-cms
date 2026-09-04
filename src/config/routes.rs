//! Custom HTTP route settings (`crap.routes.register`).

use serde::{Deserialize, Serialize};

use crate::config::parsing::serde_filesize;

/// Settings for custom Lua-defined HTTP routes.
#[derive(Debug, Clone, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
#[serde(default, deny_unknown_fields)]
pub struct RoutesConfig {
    /// URL prefix prepended to every custom route registered via
    /// `crap.routes.register`. Empty (the default) mounts each route at its
    /// literal `path`. Set e.g. `"/api"` to namespace all custom routes under a
    /// base. Must not collide with a reserved built-in prefix.
    pub prefix: String,

    /// Default request body-size limit for custom routes. A per-route
    /// `max_body` (in `crap.routes.register`) overrides this. Default: 1 MiB.
    /// Accepts integer bytes or a human string (`"512KB"`, `"2MB"`).
    #[serde(with = "serde_filesize")]
    pub max_body: u64,
}

impl Default for RoutesConfig {
    fn default() -> Self {
        Self {
            prefix: String::new(),
            max_body: 1024 * 1024,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_no_prefix_and_1mib() {
        let c = RoutesConfig::default();
        assert_eq!(c.prefix, "");
        assert_eq!(c.max_body, 1024 * 1024);
    }
}
