//! Parsing functions for collection auth configuration.
//!
//! Lua-side shape (new):
//! ```lua
//! auth = {
//!     enabled = true,
//!     token_expiry = 7200,                  -- optional; unset = [auth] token_expiry
//!     methods = {                           -- required when enabled
//!         { type = "password_login", mfa = "email", verify_email = true },
//!         { type = "bearer", surfaces = {"grpc", "admin"} },
//!         { type = "session_cookie", surfaces = {"admin"} },
//!         { type = "strategy",
//!           name = "api-key",
//!           authenticate = "hooks.auth.api_key",
//!           activates_on = { header = "x-api-key" },
//!           surfaces = {"grpc"} },
//!     },
//! }
//! ```

mod parse;
mod validate;

pub(super) use parse::parse_collection_auth;
pub(super) use validate::validate_auth_keys;
