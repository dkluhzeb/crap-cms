//! Builder for [`CollectionDefinition`](super::CollectionDefinition).

use crate::core::{
    FieldDefinition, Slug,
    collection::{
        Access, AdminConfig, Auth, CollectionDefinition, Hooks, IndexDefinition, Labels,
        LiveSetting, McpConfig, VersionsConfig,
    },
    upload::CollectionUpload,
};

/// Builder for [`CollectionDefinition`].
///
/// `slug` is taken in `new()`. All other fields default via
/// [`CollectionDefinition::default()`].
pub struct CollectionDefinitionBuilder {
    inner: CollectionDefinition,
}

impl CollectionDefinitionBuilder {
    /// Create a new `CollectionDefinitionBuilder` with the given slug.
    pub fn new(slug: impl Into<Slug>) -> Self {
        Self {
            inner: CollectionDefinition {
                slug: slug.into(),
                ..Default::default()
            },
        }
    }

    /// Set the plural and singular labels for the collection.
    pub fn labels(mut self, v: Labels) -> Self {
        self.inner.labels = v;
        self
    }

    /// Set whether the collection should include standard timestamps.
    pub fn timestamps(mut self, v: bool) -> Self {
        self.inner.timestamps = v;
        self
    }

    /// Set the field definitions for the collection.
    pub fn fields(mut self, v: Vec<FieldDefinition>) -> Self {
        self.inner.fields = v;
        self
    }

    /// Set the admin UI configuration for the collection.
    pub fn admin(mut self, v: AdminConfig) -> Self {
        self.inner.admin = v;
        self
    }

    /// Set the lifecycle hooks for the collection.
    pub fn hooks(mut self, v: Hooks) -> Self {
        self.inner.hooks = v;
        self
    }

    /// Set the authentication configuration for the collection.
    pub fn auth(mut self, v: Auth) -> Self {
        self.inner.auth = Some(v);
        self
    }

    /// Set the file upload configuration for the collection.
    pub fn upload(mut self, v: CollectionUpload) -> Self {
        self.inner.upload = Some(v);
        self
    }

    /// Set the access control rules for the collection.
    pub fn access(mut self, v: Access) -> Self {
        self.inner.access = v;
        self
    }

    /// Set the MCP-specific configuration for the collection.
    pub fn mcp(mut self, v: McpConfig) -> Self {
        self.inner.mcp = v;
        self
    }

    /// Set the live update settings for the collection.
    pub fn live(mut self, v: LiveSetting) -> Self {
        self.inner.live = Some(v);
        self
    }

    /// Set the versioning and drafts configuration for the collection.
    pub fn versions(mut self, v: VersionsConfig) -> Self {
        self.inner.versions = Some(v);
        self
    }

    /// Set additional database indexes for the collection.
    pub fn indexes(mut self, v: Vec<IndexDefinition>) -> Self {
        self.inner.indexes = v;
        self
    }

    /// Enable soft deletes for the collection.
    pub fn soft_delete(mut self, v: bool) -> Self {
        self.inner.soft_delete = v;
        self
    }

    /// Set the retention period for soft-deleted documents.
    pub fn soft_delete_retention(mut self, v: impl Into<String>) -> Self {
        self.inner.soft_delete_retention = Some(v.into());
        self
    }

    /// Build the final `CollectionDefinition` instance.
    pub fn build(self) -> CollectionDefinition {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_with_defaults() {
        let def = CollectionDefinitionBuilder::new("posts").build();
        assert_eq!(def.slug, "posts");
        assert!(def.timestamps);
        assert!(def.fields.is_empty());
        assert!(def.auth.is_none());
        assert!(def.upload.is_none());
        assert!(def.versions.is_none());
        assert!(def.indexes.is_empty());
    }

    #[test]
    fn builds_with_overrides() {
        let def = CollectionDefinitionBuilder::new("posts")
            .timestamps(false)
            .versions(VersionsConfig::new(true, 5))
            .build();
        assert_eq!(def.slug, "posts");
        assert!(!def.timestamps);
        assert!(def.versions.is_some());
        assert!(def.versions.unwrap().drafts);
    }

    #[test]
    fn builds_with_auth() {
        let def = CollectionDefinitionBuilder::new("users")
            .auth(Auth::new(true))
            .build();
        assert!(def.is_auth_collection());
    }
}
