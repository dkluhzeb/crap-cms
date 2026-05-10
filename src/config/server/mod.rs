//! Server / runtime configuration sections: bind ports, database, admin
//! UI, and Content-Security-Policy.

mod admin;
mod csp;
mod database;
mod server_config;

pub use admin::AdminConfig;
pub use database::DatabaseConfig;
pub use server_config::{CompressionMode, ServerConfig};

// `CspConfig` is reachable via `AdminConfig::csp` and isn't imported
// by name anywhere, so it stays private to the `server::admin` module.
