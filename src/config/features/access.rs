//! Access-control defaults.

use serde::{Deserialize, Serialize};

/// Access control defaults.
/// When `default_deny` is true, collections/globals without explicit access functions
/// deny all operations instead of allowing them. Default: true (secure by default).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccessConfig {
    /// When true (default), operations on collections/globals without an explicit access
    /// function are denied. When false, missing access functions allow all.
    pub default_deny: bool,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self { default_deny: true }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn access_config_default_deny_true_by_default() {
        let config = crate::config::CrapConfig::default();
        assert!(config.access.default_deny);
    }

    #[test]
    fn access_config_default_deny_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[access]\ndefault_deny = true\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert!(config.access.default_deny);
    }
}
