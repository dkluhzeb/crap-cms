//! Lua function references for collection/global lifecycle hooks.

use serde::{Deserialize, Serialize};

use crate::{core::HookRef, typegen::lua::LuaAnnotation};

/// Lua function references for lifecycle hooks.
///
/// Each entry is a [`HookRef`]: a bare ref string or a `{ ref, options }` table
/// carrying per-config options exposed to the hook as `ctx.options`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[lua(class = "crap.Hooks")]
pub struct Hooks {
    /// Hook refs to run before field validation.
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub before_validate: Vec<HookRef>,
    /// Hook refs to run before create/update write.
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub before_change: Vec<HookRef>,
    /// Hook refs to run after create/update write.
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub after_change: Vec<HookRef>,
    /// Hook refs to run before returning read results.
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub before_read: Vec<HookRef>,
    /// Hook refs to run after read, before response.
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub after_read: Vec<HookRef>,
    /// Hook refs to run before delete.
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub before_delete: Vec<HookRef>,
    /// Hook refs to run after delete.
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub after_delete: Vec<HookRef>,
    /// Hook refs to run before broadcasting live update events. Can suppress or transform event data. No CRUD access.
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub before_broadcast: Vec<HookRef>,
}

impl Hooks {
    /// Create a new default hooks configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for hooks configuration.
    #[must_use]
    pub fn builder() -> HooksBuilder {
        HooksBuilder::new()
    }
}

/// Builder for [`Hooks`]. Created via [`Hooks::builder`].
#[derive(Default)]
pub struct HooksBuilder {
    before_validate: Vec<HookRef>,
    before_change: Vec<HookRef>,
    after_change: Vec<HookRef>,
    before_read: Vec<HookRef>,
    after_read: Vec<HookRef>,
    before_delete: Vec<HookRef>,
    after_delete: Vec<HookRef>,
    before_broadcast: Vec<HookRef>,
}

impl HooksBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn before_validate(mut self, v: Vec<HookRef>) -> Self {
        self.before_validate = v;

        self
    }

    #[must_use]
    pub fn before_change(mut self, v: Vec<HookRef>) -> Self {
        self.before_change = v;

        self
    }

    #[must_use]
    pub fn after_change(mut self, v: Vec<HookRef>) -> Self {
        self.after_change = v;

        self
    }

    #[must_use]
    pub fn before_read(mut self, v: Vec<HookRef>) -> Self {
        self.before_read = v;

        self
    }

    #[must_use]
    pub fn after_read(mut self, v: Vec<HookRef>) -> Self {
        self.after_read = v;

        self
    }

    #[must_use]
    pub fn before_delete(mut self, v: Vec<HookRef>) -> Self {
        self.before_delete = v;

        self
    }

    #[must_use]
    pub fn after_delete(mut self, v: Vec<HookRef>) -> Self {
        self.after_delete = v;

        self
    }

    #[must_use]
    pub fn before_broadcast(mut self, v: Vec<HookRef>) -> Self {
        self.before_broadcast = v;

        self
    }

    #[must_use]
    pub fn build(self) -> Hooks {
        Hooks {
            before_validate: self.before_validate,
            before_change: self.before_change,
            after_change: self.after_change,
            before_read: self.before_read,
            after_read: self.after_read,
            before_delete: self.before_delete,
            after_delete: self.after_delete,
            before_broadcast: self.before_broadcast,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hooks_are_all_empty() {
        let h = Hooks::new();
        assert!(h.before_validate.is_empty());
        assert!(h.before_change.is_empty());
        assert!(h.after_change.is_empty());
        assert!(h.before_read.is_empty());
        assert!(h.after_read.is_empty());
        assert!(h.before_delete.is_empty());
        assert!(h.after_delete.is_empty());
        assert!(h.before_broadcast.is_empty());
    }

    /// Distinct value per phase, so a cross-wired assignment in `build()`
    /// would surface as a mismatch on the corresponding field.
    #[test]
    fn builder_wires_each_phase_to_its_own_field() {
        let h = Hooks::builder()
            .before_validate(vec!["bv".into()])
            .before_change(vec!["bc".into()])
            .after_change(vec!["ac".into()])
            .before_read(vec!["br".into()])
            .after_read(vec!["ar".into()])
            .before_delete(vec!["bd".into()])
            .after_delete(vec!["ad".into()])
            .before_broadcast(vec!["bb".into()])
            .build();

        assert_eq!(h.before_validate, vec![HookRef::new("bv")]);
        assert_eq!(h.before_change, vec![HookRef::new("bc")]);
        assert_eq!(h.after_change, vec![HookRef::new("ac")]);
        assert_eq!(h.before_read, vec![HookRef::new("br")]);
        assert_eq!(h.after_read, vec![HookRef::new("ar")]);
        assert_eq!(h.before_delete, vec![HookRef::new("bd")]);
        assert_eq!(h.after_delete, vec![HookRef::new("ad")]);
        assert_eq!(h.before_broadcast, vec![HookRef::new("bb")]);
    }
}
