//! Collection and global definition types parsed from Lua configuration files.

mod access;
mod admin_config;
mod auth;
mod definition;
mod global_definition;
mod hooks;
mod index_definition;
mod labels;
mod live;
mod mcp_config;
mod versions_config;

pub use access::{Access, AccessBuilder, GlobalAccess};
pub use admin_config::{AdminConfig, AdminConfigBuilder};
pub use auth::{
    Activation, Auth, AuthMethod, MfaMode, PasswordLoginBuilder, PasswordLoginCfg, StrategyCfg,
    Surface, SurfaceSet,
};
pub use definition::{CollectionDefinition, CollectionDefinitionBuilder};
pub use global_definition::{GlobalDefinition, GlobalDefinitionBuilder};
pub use hooks::{Hooks, HooksBuilder};
pub use index_definition::IndexDefinition;
pub use labels::Labels;
pub use live::{LiveMode, LiveSetting};
pub use mcp_config::{COLLECTION_OPERATIONS, GLOBAL_OPERATIONS, McpConfig};
pub use versions_config::VersionsConfig;
