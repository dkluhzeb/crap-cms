//! Admin UI behaviour, branding, CSRF, and CSP wrapper.

use serde::{Deserialize, Serialize};

use crate::config::parsing::serde_duration;
use crate::core::HookRef;

use super::csp::CspConfig;

/// Admin UI behavior settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdminConfig {
    /// Enable development mode (e.g., more verbose errors).
    pub dev_mode: bool,
    /// When true (default), block admin panel if no auth collection exists.
    /// Set to false for open dev mode with no authentication.
    pub require_auth: bool,
    /// Display name shown in the admin header wordmark and `<title>`
    /// tags. Defaults to `"Crap CMS"`. Override in `crap.toml` to
    /// rebrand without touching templates.
    #[serde(default = "default_site_name")]
    pub site_name: String,
    /// Optional Lua function ref that gates admin panel access.
    /// Checked after successful authentication. None = any authenticated user.
    /// Accepts a bare ref string or a `{ ref, options }` table (TOML inline
    /// table) carrying per-config options exposed to the hook as `ctx.options`.
    pub access: Option<HookRef>,
    /// Content-Security-Policy header configuration.
    pub csp: CspConfig,
    /// Default IANA timezone for date fields with `timezone = true` that don't
    /// specify their own `default_timezone`. Empty string means no pre-selection.
    #[serde(default)]
    pub default_timezone: String,
    /// CSRF double-submit cookie lifetime in seconds. Default: 86400 (24h).
    /// Accepts integer seconds or human strings ("4h", "30m").
    ///
    /// The cookie is tied to a `SameSite=Strict` session so longer windows
    /// carry limited replay risk; shorten if you want to reduce the window
    /// in which a stolen double-submit token can be used.
    #[serde(default = "default_csrf_cookie_lifetime", with = "serde_duration")]
    pub csrf_cookie_lifetime: u64,
}

fn default_csrf_cookie_lifetime() -> u64 {
    86400
}

fn default_site_name() -> String {
    "Crap CMS".to_string()
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            dev_mode: false,
            require_auth: true,
            site_name: default_site_name(),
            access: None,
            csp: CspConfig::default(),
            default_timezone: String::new(),
            csrf_cookie_lifetime: default_csrf_cookie_lifetime(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::config::CrapConfig;

    use super::*;

    #[test]
    fn admin_config_defaults() {
        let admin = AdminConfig::default();
        assert!(!admin.dev_mode);
        assert!(admin.require_auth);
        assert!(admin.access.is_none());
    }

    #[test]
    fn admin_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[admin]\ndev_mode = true\nrequire_auth = false\naccess = \"access.admin_panel\"\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.admin.dev_mode);
        assert!(!config.admin.require_auth);
        assert_eq!(
            config.admin.access.as_ref().map(HookRef::reference),
            Some("access.admin_panel")
        );
    }

    /// The panel-access gate accepts the `{ ref, options }` form as a TOML
    /// inline table, exposing per-config options to the gate as `ctx.options`.
    #[test]
    fn admin_access_accepts_ref_options_table() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[admin]\naccess = { ref = \"access.panel\", options = { role = \"admin\" } }\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        let access = config.admin.access.expect("access set");
        assert_eq!(access.reference(), "access.panel");
        assert_eq!(
            access.options().and_then(|o| o.get("role")),
            Some(&serde_json::json!("admin"))
        );
    }

    #[test]
    fn admin_config_partial_toml_uses_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "[admin]\ndev_mode = true\n").unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.admin.dev_mode);
        assert!(config.admin.require_auth); // default
        assert!(config.admin.access.is_none()); // default
    }

    // == CSRF cookie lifetime (audit finding M-3) ========================

    #[test]
    fn csrf_cookie_lifetime_defaults_to_24h() {
        let admin = AdminConfig::default();
        assert_eq!(admin.csrf_cookie_lifetime, 86400);
    }

    #[test]
    fn csrf_cookie_lifetime_from_toml_integer_seconds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[admin]\ncsrf_cookie_lifetime = 3600\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.admin.csrf_cookie_lifetime, 3600);
    }

    #[test]
    fn csrf_cookie_lifetime_from_toml_human_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[admin]\ncsrf_cookie_lifetime = \"4h\"\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.admin.csrf_cookie_lifetime, 4 * 3600);
    }
}
