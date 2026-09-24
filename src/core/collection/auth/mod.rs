//! Authentication configuration for collections.
//!
//! ## Model
//!
//! Each auth collection declares an ordered list of `methods`, each
//! one expressing **how** a request can prove a principal for that
//! collection. Methods come in four flavors:
//!
//! - `password_login` — enables the `Login` RPC for this collection
//!   (email + password → JWT). Carries the password-only knobs:
//!   `mfa`, `verify_email`, `forgot_password`.
//! - `bearer` — accept the JWT issued by `password_login` (or a
//!   strategy) in the `Authorization: Bearer …` header / gRPC
//!   metadata. Scoped by `surfaces`.
//! - `session_cookie` — accept the `crap_session` cookie. Admin
//!   surface only in practice. Scoped by `surfaces`.
//! - `strategy` — custom Lua authenticator. Declares its own
//!   `activates_on` discriminator (header presence, or explicit
//!   `always = true`) and its `surfaces` scope.
//!
//! At request time the auth evaluator walks every collection's
//! method list in declaration order; the first method whose
//! activation matches and whose surface includes the current
//! request wins. No implicit fallback chain across collections —
//! each method is opt-in for its own activation signal.
//!
//! ## Lua shape
//!
//! ```lua
//! crap.collections.define("users", {
//!     auth = {
//!         enabled = true,
//!         methods = crap.auth.default_methods(),  -- = password_login + bearer + session_cookie
//!     },
//! })
//!
//! crap.collections.define("service_accounts", {
//!     auth = {
//!         enabled = true,
//!         methods = {
//!             { type = "strategy",
//!               name = "svc-key",
//!               authenticate = "hooks.auth.svc_key",
//!               activates_on = { header = "x-service-key" },
//!               surfaces = {"grpc"} },
//!         },
//!     },
//! })
//! ```

mod activation;
mod config;
mod method;
mod surface;

pub use activation::Activation;
pub use config::{Auth, PasswordLoginCfg, StrategyCfg};
pub use method::{AuthMethod, MfaMode, PasswordLoginBuilder};
pub use surface::{Surface, SurfaceSet};
