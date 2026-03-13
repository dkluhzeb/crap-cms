//! Collection and global definition types parsed from Lua configuration files.

mod auth;
mod collection_definition;
mod collection_definition_builder;
mod global_definition;
mod global_definition_builder;
mod shared;

pub use auth::{Auth, AuthStrategy};
pub use collection_definition::CollectionDefinition;
pub use collection_definition_builder::CollectionDefinitionBuilder;
pub use global_definition::GlobalDefinition;
pub use global_definition_builder::GlobalDefinitionBuilder;
pub use shared::{
    Access, AccessBuilder, AdminConfig, AdminConfigBuilder, Hooks, HooksBuilder, IndexDefinition,
    Labels, LiveSetting, McpConfig, VersionsConfig,
};
