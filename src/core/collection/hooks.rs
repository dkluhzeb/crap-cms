//! Lua function references for collection/global lifecycle hooks.

use serde::{Deserialize, Serialize};

use crate::typegen::lua::LuaAnnotation;

/// Lua function references for lifecycle hooks.
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[lua(class = "crap.Hooks")]
pub struct Hooks {
    /// Hook refs to run before field validation.
    #[serde(default)]
    #[lua(optional)]
    pub before_validate: Vec<String>,
    /// Hook refs to run before create/update write.
    #[serde(default)]
    #[lua(optional)]
    pub before_change: Vec<String>,
    /// Hook refs to run after create/update write.
    #[serde(default)]
    #[lua(optional)]
    pub after_change: Vec<String>,
    /// Hook refs to run before returning read results.
    #[serde(default)]
    #[lua(optional)]
    pub before_read: Vec<String>,
    /// Hook refs to run after read, before response.
    #[serde(default)]
    #[lua(optional)]
    pub after_read: Vec<String>,
    /// Hook refs to run before delete.
    #[serde(default)]
    #[lua(optional)]
    pub before_delete: Vec<String>,
    /// Hook refs to run after delete.
    #[serde(default)]
    #[lua(optional)]
    pub after_delete: Vec<String>,
    /// Hook refs to run before broadcasting live update events. Can suppress or transform event data. No CRUD access.
    #[serde(default)]
    #[lua(optional)]
    pub before_broadcast: Vec<String>,
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
    before_validate: Vec<String>,
    before_change: Vec<String>,
    after_change: Vec<String>,
    before_read: Vec<String>,
    after_read: Vec<String>,
    before_delete: Vec<String>,
    after_delete: Vec<String>,
    before_broadcast: Vec<String>,
}

impl HooksBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn before_validate(mut self, v: Vec<String>) -> Self {
        self.before_validate = v;

        self
    }

    #[must_use]
    pub fn before_change(mut self, v: Vec<String>) -> Self {
        self.before_change = v;

        self
    }

    #[must_use]
    pub fn after_change(mut self, v: Vec<String>) -> Self {
        self.after_change = v;

        self
    }

    #[must_use]
    pub fn before_read(mut self, v: Vec<String>) -> Self {
        self.before_read = v;

        self
    }

    #[must_use]
    pub fn after_read(mut self, v: Vec<String>) -> Self {
        self.after_read = v;

        self
    }

    #[must_use]
    pub fn before_delete(mut self, v: Vec<String>) -> Self {
        self.before_delete = v;

        self
    }

    #[must_use]
    pub fn after_delete(mut self, v: Vec<String>) -> Self {
        self.after_delete = v;

        self
    }

    #[must_use]
    pub fn before_broadcast(mut self, v: Vec<String>) -> Self {
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
