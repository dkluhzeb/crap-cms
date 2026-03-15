//! Core types used across the lifecycle module.

use mlua::Lua;
use serde_json::Value;

use crate::core::Document;

/// Result of evaluating a display condition function.
#[derive(Debug, Clone)]
pub enum DisplayConditionResult {
    /// Lua returned a boolean. Must be re-evaluated server-side on changes.
    Bool(bool),
    /// Lua returned a condition table. Can be evaluated client-side.
    /// `visible` is the initial evaluation result; `condition` is the JSON to embed.
    Table { condition: Value, visible: bool },
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
///
/// Stores the fat pointer as two `usize` words (data pointer + vtable pointer)
/// to keep the struct `'static` — `mlua::set_app_data` requires `T: 'static`,
/// but the actual connection reference has a shorter lifetime. The caller
/// guarantees the connection outlives the Lua call.
pub(crate) struct TxContext {
    data: usize,
    vtable: usize,
}

impl TxContext {
    /// Construct from a reference to any `DbConnection`.
    ///
    /// # Safety
    /// The caller must call `lua.remove_app_data::<TxContext>()` before the
    /// connection referenced by `conn` is dropped. The pointer is only
    /// dereferenced inside `get_tx_conn`, which runs while the Lua VM is
    /// locked and the connection is still alive.
    pub(crate) fn new(conn: &dyn crate::db::DbConnection) -> Self {
        // Decompose the fat pointer into its two raw words.
        // `*const dyn Trait` is a (data_ptr, vtable_ptr) pair.
        // We store them as `usize` so the struct is `'static`.
        let fat_ptr: *const dyn crate::db::DbConnection = conn;
        // SAFETY: *const dyn Trait is repr(data_ptr, vtable_ptr) on all
        // supported platforms. We transmute to [usize; 2] to erase lifetimes.
        let [data, vtable]: [usize; 2] = unsafe { std::mem::transmute(fat_ptr) };
        Self { data, vtable }
    }

    /// Reconstruct the fat pointer from the stored words.
    ///
    /// # Safety
    /// Must only be called while the original connection is still alive.
    pub(crate) fn as_ptr(&self) -> *const dyn crate::db::DbConnection {
        let words: [usize; 2] = [self.data, self.vtable];
        unsafe { std::mem::transmute(words) }
    }
}

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

/// RAII guard that restores `HookDepth` to its original value on drop.
/// Prevents depth leaks when hooks return errors via `?`.
pub(crate) struct HookDepthGuard<'a> {
    lua: &'a Lua,
    original: u32,
}

impl<'a> HookDepthGuard<'a> {
    /// Increment the hook depth and return a guard that restores it on drop.
    pub(crate) fn increment(lua: &'a Lua, current: u32) -> Self {
        lua.set_app_data(HookDepth(current + 1));
        Self {
            lua,
            original: current,
        }
    }
}

impl Drop for HookDepthGuard<'_> {
    fn drop(&mut self) {
        self.lua.set_app_data(HookDepth(self.original));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

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

    #[test]
    fn hook_depth_guard_restores_on_drop() {
        let lua = Lua::new();
        lua.set_app_data(HookDepth(0));

        {
            let _guard = HookDepthGuard::increment(&lua, 0);
            assert_eq!(lua.app_data_ref::<HookDepth>().unwrap().0, 1);
        }

        assert_eq!(lua.app_data_ref::<HookDepth>().unwrap().0, 0);
    }

    #[test]
    fn hook_depth_guard_restores_on_early_exit() {
        let lua = Lua::new();
        lua.set_app_data(HookDepth(2));

        let result: Result<(), &str> = (|| {
            let _guard = HookDepthGuard::increment(&lua, 2);
            assert_eq!(lua.app_data_ref::<HookDepth>().unwrap().0, 3);
            Err("simulated error")?;
            #[allow(unreachable_code)]
            Ok(())
        })();

        assert!(result.is_err());
        assert_eq!(lua.app_data_ref::<HookDepth>().unwrap().0, 2);
    }
}
