//! `load_auth_user` — materialize an `AuthUser` from a signed JWT
//! that's already in hand (no transport extraction needed).
//!
//! Used by the MFA completion + login-callback paths where Login
//! / verify-MFA just issued a token and the next response needs
//! the full user document. The evaluator's [`load_authenticated_user`]
//! does the load-and-validate dance; this wrapper adds the
//! admin-only `ui_locale` enrichment.

use serde_json::Value;

use crate::config::LocaleConfig;
use crate::core::{AuthUser, Registry, auth::Claims};
use crate::db::{DbPool, query};
use crate::service::auth::evaluator::load_authenticated_user;

/// Load the full user document for an authenticated user. Honors
/// locked + stale-session-version rejections identically to the
/// evaluator's bearer/cookie path. Returns `None` when the user
/// can't be loaded for any reason (deleted, locked, stale).
#[cfg(not(tarpaulin_include))]
pub(crate) fn load_auth_user(
    pool: &DbPool,
    registry: &Registry,
    claims: &Claims,
    locale_config: &LocaleConfig,
) -> Option<AuthUser> {
    let conn = pool.get().ok()?;
    let mut auth = load_authenticated_user(claims, registry, &conn)?;

    auth.ui_locale = query::get_user_settings(&conn, &claims.sub)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| {
            v.get("ui_locale")
                .and_then(|l| l.as_str())
                .map(std::string::ToString::to_string)
        })
        .unwrap_or_else(|| locale_config.default_locale.clone());

    Some(auth)
}
