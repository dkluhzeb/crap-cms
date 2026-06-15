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
    /// Hook ref for reading draft (unpublished) content — a read that opts
    /// into drafts (`draft = true` / `use_draft` / `include_drafts`). Falls
    /// back to `update` when unset, so previewing a draft requires edit-level
    /// access by default (drafts are not exposed to plain readers). Set to
    /// gate draft previews behind a different policy than editing.
    #[serde(default)]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub draft: Option<HookRef>,
    /// Hook ref restricting access to version history — a *toggle*, not a
    /// per-snapshot filter. Unlike `trash`/`draft` it has NO `update` fallback:
    /// unset means **allow**, so history visibility follows the regular
    /// per-snapshot composite (`read` for published snapshots, `draft` for draft
    /// snapshots). Set it to lock the version timeline behind a stricter policy
    /// than reading the document — e.g. only editors may inspect history even
    /// though anyone may read the published doc. The function returns
    /// `true`/`false` (`ctx`-based); returning a filter table is rejected
    /// (row-level scoping is `read`'s job).
    #[serde(default)]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub versions: Option<HookRef>,
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

    /// Resolve the access function for draft (unpublished) reads.
    /// Returns `access.draft` when set, otherwise falls back to `access.update`
    /// so that previewing a draft requires edit-level access by default — a
    /// plain reader (only `access.read`) cannot pull unpublished content via
    /// the `draft = true` opt-in.
    #[must_use]
    pub fn resolve_draft(&self) -> Option<&HookRef> {
        self.draft.as_ref().or(self.update.as_ref())
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
    draft: Option<HookRef>,
    versions: Option<HookRef>,
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
    pub fn draft(mut self, draft: Option<HookRef>) -> Self {
        self.draft = draft;

        self
    }

    #[must_use]
    pub fn versions(mut self, versions: Option<HookRef>) -> Self {
        self.versions = versions;

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
            draft: self.draft,
            versions: self.versions,
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

    #[test]
    fn resolve_draft_prefers_draft_over_update() {
        let access = Access {
            draft: Some(HookRef::new("draft_fn")),
            update: Some(HookRef::new("update_fn")),
            ..Default::default()
        };
        assert_eq!(access.resolve_draft(), Some(&HookRef::new("draft_fn")));
    }

    #[test]
    fn resolve_draft_falls_back_to_update() {
        let access = Access {
            draft: None,
            update: Some(HookRef::new("update_fn")),
            ..Default::default()
        };
        assert_eq!(access.resolve_draft(), Some(&HookRef::new("update_fn")));
    }

    #[test]
    fn resolve_draft_does_not_fall_back_to_read() {
        // A plain reader (only `read` set) must NOT gain draft access — draft
        // reads require edit-level access, so with no draft/update rule this
        // resolves to None (default policy), never to `read`.
        let access = Access {
            read: Some(HookRef::new("read_fn")),
            ..Default::default()
        };
        assert!(access.resolve_draft().is_none());
    }
}
