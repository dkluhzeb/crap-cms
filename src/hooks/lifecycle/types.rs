//! Core types used across the lifecycle module.

use crate::core::Document;

/// Result of evaluating a display condition function.
#[derive(Debug, Clone)]
pub enum DisplayConditionResult {
    /// Lua returned a boolean. Must be re-evaluated server-side on changes.
    Bool(bool),
    /// Lua returned a condition table. Can be evaluated client-side.
    /// `visible` is the initial evaluation result; `condition` is the JSON to embed.
    Table {
        condition: serde_json::Value,
        visible: bool,
    },
}

/// Events that trigger hooks.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum HookEvent {
    BeforeValidate,
    BeforeChange,
    AfterChange,
    BeforeRead,
    AfterRead,
    BeforeDelete,
    AfterDelete,
    BeforeBroadcast,
    BeforeRender,
}

impl HookEvent {
    /// Return the Lua event name string for looking up registered hooks.
    pub fn as_str(&self) -> &'static str {
        match self {
            HookEvent::BeforeValidate => "before_validate",
            HookEvent::BeforeChange => "before_change",
            HookEvent::AfterChange => "after_change",
            HookEvent::BeforeRead => "before_read",
            HookEvent::AfterRead => "after_read",
            HookEvent::BeforeDelete => "before_delete",
            HookEvent::AfterDelete => "after_delete",
            HookEvent::BeforeBroadcast => "before_broadcast",
            HookEvent::BeforeRender => "before_render",
        }
    }
}

/// Events that trigger field-level hooks.
#[derive(Debug, Clone)]
pub enum FieldHookEvent {
    BeforeValidate,
    BeforeChange,
    AfterChange,
    AfterRead,
}

/// Raw pointer wrapper for injecting a transaction/connection into Lua CRUD
/// functions via `lua.set_app_data()`. Only valid between `set_app_data` and
/// `remove_app_data` calls in `run_hooks_with_conn`.
pub(crate) struct TxContext(pub(crate) *const rusqlite::Connection);

// Safety: TxContext is only stored in Lua app_data while the originating
// Connection/Transaction is alive and the Lua mutex is held. The pointer
// is never sent across threads independently.
unsafe impl Send for TxContext {}
unsafe impl Sync for TxContext {}

/// Optional authenticated user context injected alongside TxContext.
/// CRUD closures read this when overrideAccess = false.
pub(crate) struct UserContext(pub(crate) Option<Document>);
unsafe impl Send for UserContext {}
unsafe impl Sync for UserContext {}

/// Admin UI locale injected alongside TxContext/UserContext.
/// Lua hooks read this to get the current user's preferred UI language.
pub(crate) struct UiLocaleContext(pub(crate) Option<String>);

/// Tracks hook recursion depth for Lua CRUD → hook → CRUD chains.
/// Stored in Lua `app_data` alongside `TxContext`.
pub(crate) struct HookDepth(pub(crate) u32);

/// Max allowed hook depth, read from config and stored in Lua `app_data`.
pub(crate) struct MaxHookDepth(pub(crate) u32);

/// Whether the system is in default-deny mode for access control.
/// Stored in Lua `app_data` so access checks can read it without signature changes.
pub(crate) struct DefaultDeny(pub(crate) bool);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_event_names() {
        assert_eq!(HookEvent::BeforeValidate.as_str(), "before_validate");
        assert_eq!(HookEvent::BeforeChange.as_str(), "before_change");
        assert_eq!(HookEvent::AfterChange.as_str(), "after_change");
        assert_eq!(HookEvent::BeforeRead.as_str(), "before_read");
        assert_eq!(HookEvent::AfterRead.as_str(), "after_read");
        assert_eq!(HookEvent::BeforeDelete.as_str(), "before_delete");
        assert_eq!(HookEvent::AfterDelete.as_str(), "after_delete");
        assert_eq!(HookEvent::BeforeBroadcast.as_str(), "before_broadcast");
        assert_eq!(HookEvent::BeforeRender.as_str(), "before_render");
    }
}
