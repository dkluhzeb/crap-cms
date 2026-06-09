//! Relationship-population depth configuration.

use serde::{Deserialize, Serialize};

/// Controls relationship population depth defaults and limits.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DepthConfig {
    /// Default population depth when request doesn't specify one.
    /// Used as default for `FindByID`. Find defaults to 0 regardless.
    pub default_depth: i32,
    /// Maximum allowed depth application-wide. Prevents abuse.
    pub max_depth: i32,
    /// Maximum nesting depth for in-memory JSON *data structures* (the Lua↔JSON
    /// converter). Distinct from `max_depth`, which limits relationship
    /// *population*: this bounds how deeply a single value (e.g. a Lua table a
    /// hook builds) may nest, guarding against stack overflow. Must be large
    /// enough to hold the data that `max_depth` population produces — validated
    /// in [`crate::config::CrapConfig::validate`]. Generous by default.
    pub max_nesting_depth: usize,
}

impl Default for DepthConfig {
    fn default() -> Self {
        Self {
            default_depth: 1,
            max_depth: 10,
            max_nesting_depth: 64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_config_defaults() {
        let depth = DepthConfig::default();
        assert_eq!(depth.default_depth, 1);
        assert_eq!(depth.max_depth, 10);
        assert_eq!(depth.max_nesting_depth, 64);
    }
}
