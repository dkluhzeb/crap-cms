//! Builder for [`GlobalDefinition`](super::GlobalDefinition).

use crate::core::{
    FieldDefinition, Slug,
    collection::{Access, GlobalDefinition, Hooks, Labels, LiveSetting, McpConfig, VersionsConfig},
};

/// Builder for [`GlobalDefinition`].
///
/// `slug` is taken in `new()`. All other fields default via
/// [`GlobalDefinition::default()`].
pub struct GlobalDefinitionBuilder {
    inner: GlobalDefinition,
}

impl GlobalDefinitionBuilder {
    /// Create a new builder for a global with the given slug.
    pub fn new(slug: impl Into<Slug>) -> Self {
        Self {
            inner: GlobalDefinition {
                slug: slug.into(),
                ..Default::default()
            },
        }
    }

    /// Set localized labels for the global.
    pub fn labels(mut self, v: Labels) -> Self {
        self.inner.labels = v;
        self
    }

    /// Set the fields for this global.
    pub fn fields(mut self, v: Vec<FieldDefinition>) -> Self {
        self.inner.fields = v;
        self
    }

    /// Set the hooks for this global.
    pub fn hooks(mut self, v: Hooks) -> Self {
        self.inner.hooks = v;
        self
    }

    /// Set access control configuration for this global.
    pub fn access(mut self, v: Access) -> Self {
        self.inner.access = v;
        self
    }

    /// Set MCP (Model Context Protocol) configuration for this global.
    pub fn mcp(mut self, v: McpConfig) -> Self {
        self.inner.mcp = v;
        self
    }

    /// Set live update settings for this global.
    pub fn live(mut self, v: LiveSetting) -> Self {
        self.inner.live = Some(v);
        self
    }

    /// Enable and configure versioning/drafts for this global.
    pub fn versions(mut self, v: VersionsConfig) -> Self {
        self.inner.versions = Some(v);
        self
    }

    /// Build the final `GlobalDefinition`.
    pub fn build(self) -> GlobalDefinition {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_with_defaults() {
        let def = GlobalDefinitionBuilder::new("site_settings").build();
        assert_eq!(def.slug, "site_settings");
        assert!(def.fields.is_empty());
        assert!(def.versions.is_none());
    }

    #[test]
    fn builds_with_overrides() {
        let def = GlobalDefinitionBuilder::new("site_settings")
            .versions(VersionsConfig::new(true, 0))
            .build();
        assert!(def.has_drafts());
    }
}
