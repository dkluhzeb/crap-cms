//! Lua function references for collection/global access control.

use serde::{Deserialize, Serialize};

/// Lua function references for access control (read/create/update/delete).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Access {
    /// Lua function for read access control.
    #[serde(default)]
    pub read: Option<String>,
    /// Lua function for create access control.
    #[serde(default)]
    pub create: Option<String>,
    /// Lua function for update access control.
    #[serde(default)]
    pub update: Option<String>,
    /// Lua function for delete access control.
    #[serde(default)]
    pub delete: Option<String>,
    /// Lua function for trash access control (soft delete + restore).
    /// Only relevant for collections with `soft_delete = true`.
    /// When not set, falls back to `access.update`.
    #[serde(default)]
    pub trash: Option<String>,
}

impl Access {
    /// Create a new default access control configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for access control configuration.
    pub fn builder() -> AccessBuilder {
        AccessBuilder::new()
    }

    /// Resolve the access function for trash operations (soft delete + restore).
    /// Returns `access.trash` when set, otherwise falls back to `access.update`.
    pub fn resolve_trash(&self) -> Option<&str> {
        self.trash.as_deref().or(self.update.as_deref())
    }
}

/// Builder for [`Access`]. Created via [`Access::builder`].
#[derive(Default)]
pub struct AccessBuilder {
    read: Option<String>,
    create: Option<String>,
    update: Option<String>,
    delete: Option<String>,
    trash: Option<String>,
}

impl AccessBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub fn read(mut self, read: Option<String>) -> Self {
        self.read = read;

        self
    }

    pub fn create(mut self, create: Option<String>) -> Self {
        self.create = create;

        self
    }

    pub fn update(mut self, update: Option<String>) -> Self {
        self.update = update;

        self
    }

    pub fn delete(mut self, delete: Option<String>) -> Self {
        self.delete = delete;

        self
    }

    pub fn trash(mut self, trash: Option<String>) -> Self {
        self.trash = trash;

        self
    }

    pub fn build(self) -> Access {
        Access {
            read: self.read,
            create: self.create,
            update: self.update,
            delete: self.delete,
            trash: self.trash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_trash_prefers_trash_over_update() {
        let access = Access {
            trash: Some("trash_fn".to_string()),
            update: Some("update_fn".to_string()),
            ..Default::default()
        };
        assert_eq!(access.resolve_trash(), Some("trash_fn"));
    }

    #[test]
    fn resolve_trash_falls_back_to_update() {
        let access = Access {
            trash: None,
            update: Some("update_fn".to_string()),
            ..Default::default()
        };
        assert_eq!(access.resolve_trash(), Some("update_fn"));
    }

    #[test]
    fn resolve_trash_returns_none_when_both_unset() {
        let access = Access::default();
        assert!(access.resolve_trash().is_none());
    }
}
