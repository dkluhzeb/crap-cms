//! Builder for [`GlobalDefinition`](super::GlobalDefinition).

use super::{Access, GlobalDefinition, Hooks, Labels, LiveSetting, McpConfig, VersionsConfig};
use crate::core::field::FieldDefinition;

/// Builder for [`GlobalDefinition`].
///
/// `slug` is taken in `new()`. All other fields default via
/// [`GlobalDefinition::default()`].
pub struct GlobalDefinitionBuilder {
    inner: GlobalDefinition,
}

impl GlobalDefinitionBuilder {
    pub fn new(slug: impl Into<String>) -> Self {
        Self {
            inner: GlobalDefinition {
                slug: slug.into(),
                ..Default::default()
            },
        }
    }

    pub fn labels(mut self, v: Labels) -> Self {
        self.inner.labels = v;
        self
    }

    pub fn fields(mut self, v: Vec<FieldDefinition>) -> Self {
        self.inner.fields = v;
        self
    }

    pub fn hooks(mut self, v: Hooks) -> Self {
        self.inner.hooks = v;
        self
    }

    pub fn access(mut self, v: Access) -> Self {
        self.inner.access = v;
        self
    }

    pub fn mcp(mut self, v: McpConfig) -> Self {
        self.inner.mcp = v;
        self
    }

    pub fn live(mut self, v: LiveSetting) -> Self {
        self.inner.live = Some(v);
        self
    }

    pub fn versions(mut self, v: VersionsConfig) -> Self {
        self.inner.versions = Some(v);
        self
    }

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
