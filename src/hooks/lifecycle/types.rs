//! Core types used across the lifecycle module.

use std::sync::Arc;

use mlua::Lua;

use crate::{
    config::LocaleConfig,
    core::{
        ConditionExpr, Document, Registry, SharedCache, SharedEventTransport,
        SharedInvalidationTransport, SharedStorage,
    },
    db::{DbConnection, DbPool},
    service::{EventQueue, ServiceContext, VerificationQueue},
    typegen::lua::LuaAlias,
};

/// Result of evaluating a display condition function.
#[derive(Debug, Clone)]
pub enum DisplayConditionResult {
    /// Lua returned a boolean. Must be re-evaluated server-side on changes.
    Bool(bool),
    /// Lua returned a condition table parseable into a typed [`ConditionExpr`].
    /// Can be evaluated client-side. `visible` is the initial evaluation
    /// result; `condition` is the typed shape that serializes to the same
    /// JSON the JS evaluator expects.
    Table {
        condition: ConditionExpr,
        visible: bool,
    },
}

/// Events that trigger hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LuaAlias)]
#[lua(alias = "crap.HookEvent", rename_all = "snake_case")]
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
    #[must_use]
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
#[derive(Clone, Copy)]
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
    pub(crate) fn new(conn: &dyn DbConnection) -> Self {
        // Decompose the fat pointer into its two raw words.
        // `*const dyn Trait` is a (data_ptr, vtable_ptr) pair.
        // We store them as `usize` so the struct is `'static`.
        let fat_ptr: *const dyn DbConnection = conn;
        // SAFETY: *const dyn Trait is repr(data_ptr, vtable_ptr) on all
        // supported platforms. We transmute to [usize; 2] to erase lifetimes.
        let [data, vtable]: [usize; 2] = unsafe { std::mem::transmute(fat_ptr) };
        Self { data, vtable }
    }

    /// Reconstruct the fat pointer from the stored words.
    ///
    /// # Safety
    /// Must only be called while the original connection is still alive.
    pub(crate) fn as_ptr(&self) -> *const dyn DbConnection {
        let words: [usize; 2] = [self.data, self.vtable];
        unsafe { std::mem::transmute(words) }
    }
}

// Safety: TxContext is only stored in Lua app_data while the originating
// Connection/Transaction is alive and the Lua mutex is held. The pointer
// is never sent across threads independently.
unsafe impl Send for TxContext {}
unsafe impl Sync for TxContext {}

/// Owns a clonable `DbPool` for **pool-mode** dispatch. Set in Lua
/// `app_data` by job handlers (`run_job_handler`) where each Lua CRUD
/// operation should open its own short-lived IMMEDIATE transaction
/// instead of using a shared outer tx. Distinguishes job context from
/// hook context — hooks set `TxContext` (shared tx); jobs set
/// `PoolContext` (per-op tx).
///
/// The `crap.transaction(fn)` Lua API temporarily swaps `PoolContext`
/// for `TxContext` during `fn` so a sequence of CRUD ops shares one
/// tx for explicit multi-step atomicity.
#[derive(Clone)]
pub(crate) struct PoolContext(pub(crate) DbPool);

/// Optional authenticated user context injected alongside `TxContext`.
/// CRUD closures read this when overrideAccess = false.
#[derive(Clone)]
pub(crate) struct UserContext(pub(crate) Option<Document>);
unsafe impl Send for UserContext {}
unsafe impl Sync for UserContext {}

/// Admin UI locale injected alongside TxContext/UserContext.
/// Lua hooks read this to get the current user's preferred UI language.
#[derive(Clone)]
pub(crate) struct UiLocaleContext(pub(crate) Option<String>);

/// Maximum Lua instructions per hook invocation. Stored in `app_data`.
pub(crate) struct MaxInstructions(pub(crate) u64);

/// VM-stable infrastructure bundle, set once in Lua `app_data` at VM build
/// (`create_lua_vm`) and read by the CRUD and access layers. Bundles what used
/// to be six separate app-data newtypes so a VM can't end up with a partial
/// set. Everything here is stable for the VM's lifetime; per-call state
/// (`TxContext`, `UserContext`, `LuaCrudInfra`, `HookDepth`) stays separate.
///
/// This is the Lua analogue of the process-wide [`crate::service::AppInfra`]:
/// the values originate from the same boot wiring (`HookRunner::builder`), but
/// a VM cannot hold `AppInfra` itself — `AppInfra` owns the `HookRunner`,
/// which owns the VMs (an `Arc` cycle, and `AppInfra` is assembled after the
/// runner), and `storage` is genuinely per-VM: a `Custom` upload backend
/// delegates to THIS VM via a `LocalLease` so CRUD-delete doesn't re-acquire
/// from the pool (which would deadlock).
pub(crate) struct LuaVmInfra {
    /// Registry snapshot — resolves collection/global defs for the inline
    /// Lua-CRUD path and the access chokepoint (which has no `HookRunner`
    /// handle to ask).
    pub(crate) registry: Arc<Registry>,
    /// Locale configuration, so Lua CRUD write paths (notably `unpublish`)
    /// can build a default `LocaleContext` for raw reads of localized fields.
    /// Without it the service layer falls back to bare column names and
    /// `SQLite` errors with `no such column`.
    pub(crate) locale_config: LocaleConfig,
    /// Per-VM storage backend for upload file cleanup in CRUD hooks (see the
    /// struct docs for why it's per-VM). `None` only in tests.
    pub(crate) storage: Option<SharedStorage>,
    /// User-invalidation transport so CRUD delete and lock paths can publish
    /// live-stream tear-down signals from Lua-invoked service calls.
    /// `None` = no-op.
    pub(crate) invalidation_transport: Option<SharedInvalidationTransport>,
    /// Max allowed hook recursion depth, from `[hooks] max_depth`.
    pub(crate) max_hook_depth: u32,
    /// Whether the system is in default-deny mode for access control.
    pub(crate) default_deny: bool,
}

impl Default for LuaVmInfra {
    /// Test convenience: an empty registry, default locale, no storage or
    /// transports, `max_hook_depth = 3`, allow-by-default access.
    fn default() -> Self {
        Self {
            registry: Arc::new(Registry::default()),
            locale_config: LocaleConfig::default(),
            storage: None,
            invalidation_transport: None,
            max_hook_depth: 3,
            default_deny: false,
        }
    }
}

/// Infrastructure for Lua CRUD event publishing, cache invalidation, and event
/// queueing. Stored in Lua `app_data` alongside `TxContext` so that CRUD
/// functions called from hooks can build a complete `ServiceContext` with event
/// transport, cache, and the parent's event queue.
///
/// When present, Lua CRUD operations queue events into the parent's
/// `event_queue` (flushed after the parent's transaction commits) and
/// invalidate the populate cache on writes.
#[derive(Clone)]
pub struct LuaCrudInfra {
    pub event_transport: Option<SharedEventTransport>,
    pub cache: Option<SharedCache>,
    pub event_queue: Option<EventQueue>,
    pub verification_queue: Option<VerificationQueue>,
}

impl LuaCrudInfra {
    /// Build from a parent `ServiceContext`, attaching the given queues.
    /// Clones the context's event transport and cache (cheap Arc clones).
    #[must_use]
    pub fn from_ctx(
        ctx: &ServiceContext,
        event_queue: Option<EventQueue>,
        verification_queue: Option<VerificationQueue>,
    ) -> Self {
        Self {
            event_transport: ctx.event_transport.clone(),
            cache: ctx.cache.clone(),
            event_queue,
            verification_queue,
        }
    }
}

// Safety: LuaCrudInfra contains Arc (Send+Sync) and Rc<RefCell> (not Send).
// The Rc<RefCell<Vec>> (EventQueue) is only accessed on the same thread that
// owns the Lua VM — mlua's `send` feature enforces this at the type level.
// We need Send+Sync for `set_app_data` but the Rc never crosses threads.
unsafe impl Send for LuaCrudInfra {}
unsafe impl Sync for LuaCrudInfra {}

/// Tracks hook recursion depth for Lua CRUD → hook → CRUD chains.
/// Stored in Lua `app_data` alongside `TxContext`.
pub(crate) struct HookDepth(pub(crate) u32);

/// Marker stored in Lua `app_data` for the duration of init-time loading
/// (collection / global / job def files plus `init.lua`). APIs that only
/// make sense at init time — `crap.pages.register`,
/// `crap.template_data.register`, etc. — check for this marker and refuse
/// when called from a runtime hook. Without this, runtime registration
/// would either no-op silently (custom pages — read once at startup) or
/// fragment across the Lua VM pool (template data — registered into one
/// VM only), both of which are footguns for plugin authors.
pub struct InitPhase;

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

/// RAII guard for the per-VM hook context (`TxContext` / `PoolContext`,
/// `UserContext`, `UiLocaleContext`, `LuaCrudInfra`).
///
/// **Stack discipline (save/restore).** On construction the guard *snapshots*
/// the current value of every slot it manages; on drop it *restores* those
/// snapshots rather than blindly removing them. This makes the context
/// **composable under nesting**: a guard set on a VM that already carries an
/// outer context (e.g. `set_identity` on a VM mid-hook, or a nested hook) puts
/// the parent's context back when it drops, instead of tearing it down. This is
/// the same pattern as [`HookDepthGuard`], generalized to the whole context.
///
/// Restoring is sound for the raw-pointer `TxContext` because guards are
/// lexically nested: the parent's connection always outlives the child guard,
/// so the restored pointer is still live.
pub(crate) struct TxContextGuard<'a> {
    lua: &'a Lua,
    prev_tx: Option<TxContext>,
    prev_pool: Option<PoolContext>,
    prev_user: Option<UserContext>,
    prev_locale: Option<UiLocaleContext>,
    prev_infra: Option<LuaCrudInfra>,
}

impl<'a> TxContextGuard<'a> {
    /// Snapshot the current value of every managed slot (for restore on drop).
    /// In practice these are all `None` at the real call sites (guards are
    /// installed at pool entry on a fresh VM), so this is near-free; it only
    /// allocates when something is genuinely being shadowed by nesting.
    fn snapshot(lua: &'a Lua) -> Self {
        Self {
            lua,
            prev_tx: lua.app_data_ref::<TxContext>().map(|r| *r),
            prev_pool: lua.app_data_ref::<PoolContext>().map(|r| (*r).clone()),
            prev_user: lua.app_data_ref::<UserContext>().map(|r| (*r).clone()),
            prev_locale: lua.app_data_ref::<UiLocaleContext>().map(|r| (*r).clone()),
            prev_infra: lua.app_data_ref::<LuaCrudInfra>().map(|r| (*r).clone()),
        }
    }

    /// Set `TxContext` (conn-mode), `UserContext`, `UiLocaleContext`,
    /// and optionally `LuaCrudInfra`, returning a guard that restores the
    /// previous context on drop. Used by hooks and `crap.transaction(fn)` —
    /// all callers where a single shared outer transaction is the right model.
    pub(crate) fn set(
        lua: &'a Lua,
        conn: &dyn DbConnection,
        user: Option<Document>,
        ui_locale: Option<String>,
        infra: Option<LuaCrudInfra>,
    ) -> Self {
        let guard = Self::snapshot(lua);

        lua.set_app_data(TxContext::new(conn));
        lua.set_app_data(UserContext(user));
        lua.set_app_data(UiLocaleContext(ui_locale));

        if let Some(infra) = infra {
            lua.set_app_data(infra);
        }

        guard
    }

    /// Set `PoolContext` (pool-mode), `UserContext`, `UiLocaleContext`,
    /// and optionally `LuaCrudInfra`. Used by job handlers — each Lua
    /// CRUD operation will open its own short-lived IMMEDIATE
    /// transaction via the pool path (see
    /// `hooks::lua_api::crud::tx_conn::with_lua_db`). Avoids the
    /// `SQLITE_BUSY_SNAPSHOT` hazard that fires when a long-running
    /// handler's read snapshot conflicts with concurrent writers.
    pub(crate) fn set_pool(
        lua: &'a Lua,
        pool: DbPool,
        user: Option<Document>,
        ui_locale: Option<String>,
        infra: Option<LuaCrudInfra>,
    ) -> Self {
        let guard = Self::snapshot(lua);

        lua.set_app_data(PoolContext(pool));
        lua.set_app_data(UserContext(user));
        lua.set_app_data(UiLocaleContext(ui_locale));

        if let Some(infra) = infra {
            lua.set_app_data(infra);
        }

        guard
    }

    /// Set only `UserContext` and `UiLocaleContext` (no `TxContext`/`PoolContext`,
    /// so NO CRUD access). Used by the `after_read` and validation paths, where
    /// field hooks and validators should see the current user + admin UI locale
    /// but must not be given a transaction/pool to run CRUD against. Thanks to
    /// the save/restore discipline this is safe to call even on a VM that
    /// already carries an outer context — it restores it on drop.
    pub(crate) fn set_identity(
        lua: &'a Lua,
        user: Option<Document>,
        ui_locale: Option<String>,
    ) -> Self {
        let guard = Self::snapshot(lua);

        lua.set_app_data(UserContext(user));
        lua.set_app_data(UiLocaleContext(ui_locale));

        guard
    }
}

impl Drop for TxContextGuard<'_> {
    fn drop(&mut self) {
        // Restore the previous values (stack discipline) rather than removing,
        // so a nested guard cannot tear down its parent's context.
        restore_slot(self.lua, self.prev_tx.take());
        restore_slot(self.lua, self.prev_pool.take());
        restore_slot(self.lua, self.prev_user.take());
        restore_slot(self.lua, self.prev_locale.take());
        restore_slot(self.lua, self.prev_infra.take());
    }
}

/// Restore one app-data slot to a snapshotted value: re-install it when the
/// slot was previously set, otherwise remove it (it was absent before).
fn restore_slot<T: Send + Sync + 'static>(lua: &Lua, prev: Option<T>) {
    match prev {
        Some(v) => {
            lua.set_app_data(v);
        }
        None => {
            lua.remove_app_data::<T>();
        }
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

    fn current_user_id(lua: &Lua) -> Option<String> {
        lua.app_data_ref::<UserContext>()
            .and_then(|r| r.0.as_ref().map(|d| d.id.to_string()))
    }

    /// Stack discipline: a nested context guard must **restore** the parent's
    /// context on drop, not remove it. This is the regression test for the
    /// Example-B clobbering bug — without save/restore, dropping the inner
    /// guard would wipe `UserContext`, leaving the outer scope with `nil`.
    #[test]
    fn context_guard_restores_outer_context_on_nested_drop() {
        let lua = Lua::new();
        let alice = Document::new("alice");
        let bob = Document::new("bob");

        let outer = TxContextGuard::set_identity(&lua, Some(alice), Some("en".to_string()));
        assert_eq!(current_user_id(&lua).as_deref(), Some("alice"));

        {
            // Nested scope shadows the user with bob.
            let _inner = TxContextGuard::set_identity(&lua, Some(bob), None);
            assert_eq!(current_user_id(&lua).as_deref(), Some("bob"));
            // `_inner` drops here — must put alice back, not remove the slot.
        }

        assert_eq!(
            current_user_id(&lua).as_deref(),
            Some("alice"),
            "nested guard must restore the outer user, not tear it down"
        );
        assert_eq!(
            lua.app_data_ref::<UiLocaleContext>()
                .and_then(|r| r.0.clone())
                .as_deref(),
            Some("en"),
            "outer ui_locale must survive a nested scope"
        );

        drop(outer);
        assert!(
            lua.app_data_ref::<UserContext>().is_none(),
            "outermost guard removes the slot (nothing was there before)"
        );
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

        let result: Result<(), &str> = {
            let _guard = HookDepthGuard::increment(&lua, 2);
            assert_eq!(lua.app_data_ref::<HookDepth>().unwrap().0, 3);
            Err("simulated error")
        };

        assert!(result.is_err());
        assert_eq!(lua.app_data_ref::<HookDepth>().unwrap().0, 2);
    }

    /// Regression: the VM-infra storage must be retrievable from Lua `app_data`
    /// so that `delete/delete_many` CRUD functions can clean up upload files.
    #[test]
    fn lua_vm_infra_storage_stored_and_retrieved() {
        use crate::core::upload::storage::LocalStorage;

        let lua = Lua::new();
        let storage: SharedStorage = Arc::new(LocalStorage::new("/tmp/test-uploads"));
        lua.set_app_data(LuaVmInfra {
            storage: Some(storage),
            ..Default::default()
        });

        let retrieved = lua.app_data_ref::<LuaVmInfra>().unwrap();
        assert_eq!(retrieved.storage.as_ref().unwrap().kind(), "local");
    }
}
