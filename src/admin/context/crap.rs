//! `crap` metadata block — version, build hash, dev-mode flag, auth flag, CSP nonce.
//!
//! Available to every template at `{{crap.*}}`. The CSP nonce is read from a
//! task-local set by the security_headers middleware so inline `<script>` tags
//! in built-in and overlay templates pass CSP.

use schemars::JsonSchema;
use serde::Serialize;

use crate::admin::{AdminState, csp_nonce::current_nonce_or_empty};

/// Metadata about the running crap-cms process and current request.
#[derive(Serialize, JsonSchema)]
pub struct CrapMeta {
    /// Crate version (Cargo.toml `version`).
    pub version: &'static str,
    /// Build hash (set by build script from git).
    pub build_hash: &'static str,
    /// Whether admin dev-mode is enabled (per-request template reload, etc.).
    pub dev_mode: bool,
    /// Whether the system has any auth-enabled collections.
    pub auth_enabled: bool,
    /// Per-request CSP nonce (empty string outside request scope).
    pub csp_nonce: String,
}

impl CrapMeta {
    /// Build from admin state. `auth_enabled` is derived from whether any
    /// collection has auth turned on.
    pub fn from_state(state: &AdminState) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            build_hash: env!("BUILD_HASH"),
            dev_mode: state.config.admin.dev_mode,
            auth_enabled: has_auth_collections(state),
            csp_nonce: current_nonce_or_empty(),
        }
    }

    /// Build for an auth-page render. `auth_enabled` is hard-coded `true` —
    /// auth pages only render when auth is configured for the system.
    pub fn for_auth_page(state: &AdminState) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            build_hash: env!("BUILD_HASH"),
            dev_mode: state.config.admin.dev_mode,
            auth_enabled: true,
            csp_nonce: current_nonce_or_empty(),
        }
    }
}

fn has_auth_collections(state: &AdminState) -> bool {
    state
        .registry
        .collections
        .values()
        .any(|def| def.is_auth_collection())
}
