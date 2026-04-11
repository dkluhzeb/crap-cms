//! Collection and global definition types parsed from Lua configuration files.

mod access_builder;
mod admin_config_builder;
mod auth;
mod collection_definition;
mod collection_definition_builder;
mod global_definition;
mod global_definition_builder;
mod hooks_builder;
mod shared;

pub use access_builder::AccessBuilder;
pub use admin_config_builder::AdminConfigBuilder;
pub use auth::{Auth, AuthStrategy, MfaMode};
pub use collection_definition::CollectionDefinition;
pub use collection_definition_builder::CollectionDefinitionBuilder;
pub use global_definition::GlobalDefinition;
pub use global_definition_builder::GlobalDefinitionBuilder;
pub use hooks_builder::HooksBuilder;
pub use shared::{
    Access, AdminConfig, Hooks, IndexDefinition, Labels, LiveMode, LiveSetting, McpConfig,
    VersionsConfig,
};
