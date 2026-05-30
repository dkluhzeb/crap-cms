//! Server / runtime configuration sections: bind ports, database, admin
//! UI, and Content-Security-Policy.

mod admin;
mod config;
mod csp;
mod database;

pub use admin::AdminConfig;
pub use config::{CompressionMode, ServerConfig};
pub use database::{DatabaseBackend, DatabaseConfig};

// `CspConfig` is reachable via `AdminConfig::csp` and isn't imported
// by name anywhere, so it stays private to the `server::admin` module.
