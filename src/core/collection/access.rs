//! Lua function references for collection/global access control.

use serde::{Deserialize, Serialize};

use crate::{core::HookRef, typegen::lua::LuaAnnotation};

/// Lua function references for access control (read/create/update/delete).
///
/// Each rule is a [`HookRef`]: a bare ref string or a `{ ref, options }` table
/// whose options reach the access function as `ctx.options`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[lua(class = "crap.Access")]
pub struct Access {
    /// Hook ref for read access control.
    #[serde(default)]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub read: Option<HookRef>,
    /// Hook ref for create access control.
    #[serde(default)]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub create: Option<HookRef>,
    /// Hook ref for update access control.
    #[serde(default)]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub update: Option<HookRef>,
    /// Hook ref for delete access control.
    #[serde(default)]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub delete: Option<HookRef>,
    /// Hook ref for soft-delete (trash) access control. Falls back to
    /// `update` when unset, so most collections don't set this
    /// explicitly. Set to lock trashing behind a different policy
    /// than update — e.g. only editors can trash, but authors can
    /// still update their own drafts.
    #[serde(default)]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub trash: Option<HookRef>,
}

impl Access {
    /// Create a new default access control configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for access control configuration.
    #[must_use]
    pub fn builder() -> AccessBuilder {
        AccessBuilder::new()
    }

    /// Resolve the access function for trash operations (soft delete + restore).
    /// Returns `access.trash` when set, otherwise falls back to `access.update`.
    #[must_use]
    pub fn resolve_trash(&self) -> Option<&HookRef> {
        self.trash.as_ref().or(self.update.as_ref())
    }
}

/// Builder for [`Access`]. Created via [`Access::builder`].
#[derive(Default)]
pub struct AccessBuilder {
    read: Option<HookRef>,
    create: Option<HookRef>,
    update: Option<HookRef>,
    delete: Option<HookRef>,
    trash: Option<HookRef>,
}

impl AccessBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn read(mut self, read: Option<HookRef>) -> Self {
        self.read = read;

        self
    }

    #[must_use]
    pub fn create(mut self, create: Option<HookRef>) -> Self {
        self.create = create;

        self
    }

    #[must_use]
    pub fn update(mut self, update: Option<HookRef>) -> Self {
        self.update = update;

        self
    }

    #[must_use]
    pub fn delete(mut self, delete: Option<HookRef>) -> Self {
        self.delete = delete;

        self
    }

    #[must_use]
    pub fn trash(mut self, trash: Option<HookRef>) -> Self {
        self.trash = trash;

        self
    }

    #[must_use]
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
            trash: Some(HookRef::new("trash_fn")),
            update: Some(HookRef::new("update_fn")),
            ..Default::default()
        };
        assert_eq!(access.resolve_trash(), Some(&HookRef::new("trash_fn")));
    }

    #[test]
    fn resolve_trash_falls_back_to_update() {
        let access = Access {
            trash: None,
            update: Some(HookRef::new("update_fn")),
            ..Default::default()
        };
        assert_eq!(access.resolve_trash(), Some(&HookRef::new("update_fn")));
    }

    #[test]
    fn resolve_trash_returns_none_when_both_unset() {
        let access = Access::default();
        assert!(access.resolve_trash().is_none());
    }
}
