//! `HookRunner` methods for the auth hooks: custom strategies, auth
//! callbacks, and the `password_login` method's `mfa_when` / `mfa_deliver`.
//!
//! Each hook's CRUD contract follows what it is for:
//!
//! - A **strategy** (at login, or per request), an **auth callback** and an
//!   **`mfa_deliver`** hook run their CRUD in one transaction on a write-pool
//!   connection that opens lazily ([`LazyTx`]): at a callback's or
//!   `mfa_deliver` hook's first CRUD call, at a strategy's first write — its
//!   reads before that run on the caller's connection. Outbound HTTP never
//!   pins a write connection unless the hook wrote before it. The
//!   transaction gets the full transaction scope every other write surface
//!   has ([`TxScope`]): live
//!   events, populate-cache invalidation, account verification, upload file
//!   cleanup and `crap.tx` effects, all delivered only after it commits.
//! - **`mfa_when`** is a read-only predicate on the caller's connection.

use std::{cell::RefCell, rc::Rc};

use anyhow::Result;
use mlua::{Lua, Table, Value};

use crate::{
    core::{Document, HookRef, document::DocumentBuilder},
    db::DbConnection,
    hooks::{
        HookRunner, LuaCrudInfra,
        lifecycle::{
            AuthStrategyContext, AuthStrategyInput, LazyTx, LazyTxGuard, MfaDeliverContext,
            MfaDeliverInput, MfaWhenContext, MfaWhenInput, ReadOnlyScopeGuard, TxContextGuard,
            converters::lua_table_to_json_map, execution::resolve_hook_function,
        },
        lua_api::{to_lua_value, transaction::TxScope},
    },
    service::{AppInfra, EventQueue, ServiceContext, flush_queue},
};

/// Transaction label of a strategy run on the caller's connection.
const STRATEGY_TX: &str = "auth-strategy";

/// An auth hook's transaction and the infrastructure its writes publish
/// through.
struct AuthHookTx<'a> {
    tx: LazyTx<'a>,
    infra: &'a AppInfra,
}

/// Convert a Lua table returned by an auth strategy into a Document.
fn lua_table_to_auth_user(tbl: &Table) -> Result<Document> {
    let id: String = tbl.get("id")?;

    // Reuse the shared table→map converter (the inverse of
    // `document_to_lua_table`), then drop the reserved keys carried separately.
    let mut fields = lua_table_to_json_map(tbl)?;
    fields.remove("id");
    fields.remove("created_at");
    fields.remove("updated_at");

    let created_at: Option<String> = tbl.get("created_at").ok();
    let updated_at: Option<String> = tbl.get("updated_at").ok();

    Ok(DocumentBuilder::new(id)
        .fields(fields)
        .created_at(created_at)
        .updated_at(updated_at)
        .build())
}

/// Call a strategy (or callback) function on `lua`, whose database context
/// the caller installed, and read its verdict: the user table it returned,
/// or `None`.
fn call_auth_strategy(
    lua: &Lua,
    authenticate: &HookRef,
    input: &AuthStrategyInput,
) -> Result<Option<Document>> {
    let func = resolve_hook_function(lua, authenticate.reference())?;

    // Build context table from a typed Rust struct so the Lua-side
    // shape is the single source of truth (see
    // `hooks::lifecycle::AuthStrategyContext`).
    let ctx = AuthStrategyContext {
        headers: input.headers,
        collection: input.collection,
        email: input.email,
        password: input.password,
        remote_addr: input.remote_addr,
        options: authenticate.options(),
    };
    let ctx_value = to_lua_value(lua, &ctx)?;

    let result: Value = func.call(ctx_value)?;

    match result {
        Value::Table(tbl) => Ok(Some(lua_table_to_auth_user(&tbl)?)),
        _ => Ok(None),
    }
}

/// Call an `mfa_deliver` hook on `lua`, whose database context the caller
/// installed. Its return value is ignored.
fn call_mfa_deliver(lua: &Lua, hook: &HookRef, input: &MfaDeliverInput) -> Result<()> {
    let func = resolve_hook_function(lua, hook.reference())?;

    let ctx = MfaDeliverContext {
        collection: input.collection,
        user: &input.user.fields,
        code: input.code,
        expires_in: input.expires_in,
        options: hook.options(),
    };
    let ctx_value = to_lua_value(lua, &ctx)?;

    let _: Value = func.call(ctx_value)?;

    Ok(())
}

impl HookRunner {
    /// Run `call` on a pooled VM inside `hook_tx`'s transaction scope, and
    /// commit the transaction when `commit_if` accepts the hook's result
    /// (rolled back otherwise, and on every error). Events the committed
    /// transaction produced are published after the VM is released — their
    /// `before_broadcast` hooks acquire a VM of their own.
    fn run_in_auth_tx<R>(
        &self,
        hook_tx: AuthHookTx<'_>,
        call: impl FnOnce(&Lua) -> Result<R>,
        commit_if: impl FnOnce(&R) -> bool,
    ) -> Result<R> {
        let events: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let transport = hook_tx.infra.event_transport.clone();

        let result = self.run_auth_tx_in_vm(hook_tx, &events, call, commit_if);

        let flush_ctx = ServiceContext::slug_only("")
            .runner(self)
            .event_transport(transport)
            .build();
        flush_queue(&flush_ctx, &events);

        result
    }

    /// The VM-holding body of [`Self::run_in_auth_tx`].
    fn run_auth_tx_in_vm<R>(
        &self,
        hook_tx: AuthHookTx<'_>,
        events: &EventQueue,
        call: impl FnOnce(&Lua) -> Result<R>,
        commit_if: impl FnOnce(&R) -> bool,
    ) -> Result<R> {
        let lua = self.pool.acquire()?;

        let mut crud = LuaCrudInfra::for_pool_crud(hook_tx.infra);
        crud.event_queue = Some(events.clone());

        let _ambient = TxContextGuard::set_hook_tx(&lua, hook_tx.infra.pool.clone(), crud);

        let tx = hook_tx.tx;
        let scope = TxScope::open(&lua, tx.label());

        let result = {
            let _lazy = LazyTxGuard::install(&lua, &tx);

            call(&lua)
        };

        let commit = result.as_ref().is_ok_and(commit_if);

        scope.settle(tx, result, commit, |e| e)
    }

    /// Run a custom auth strategy function. Returns the user it
    /// authenticates, if any.
    ///
    /// Its reads run on `conn` — the login's connection, or the request's own
    /// read connection for a per-request strategy — until it writes: its
    /// first write opens a transaction on a write-pool connection (so a
    /// strategy that only looks its user up never takes one, and none is held
    /// across its network I/O before it writes), which every later call
    /// shares and which COMMITS only when it authenticates someone. A
    /// failed or erroring attempt rolls back — strategy attempts are
    /// attacker-controlled (unauthenticated input), so persistent writes
    /// keyed to failures would let anyone grow the database from the login
    /// endpoint. Counters belong in the rate limiters, observability in
    /// `crap.log` (neither lives in this transaction).
    ///
    /// `infra` supplies the write pool and the transports the strategy's
    /// writes publish through.
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition, the transaction, function
    /// resolution, or the strategy call itself fails.
    pub fn run_auth_strategy(
        &self,
        authenticate: &HookRef,
        input: &AuthStrategyInput,
        conn: &dyn DbConnection,
        infra: &AppInfra,
    ) -> Result<Option<Document>> {
        let hook_tx = AuthHookTx {
            tx: LazyTx::on_pool_reading(infra.pool.clone(), conn, STRATEGY_TX),
            infra,
        };

        self.run_in_auth_tx(
            hook_tx,
            |lua| call_auth_strategy(lua, authenticate, input),
            Option::is_some,
        )
    }

    /// Run an auth-callback hook (OAuth / OIDC: `auth_callback/{name}.lua`).
    /// Returns the user it authenticates, if any.
    ///
    /// Same contract as [`Self::run_auth_strategy`] — its writes (typically
    /// provisioning the user on first sign-in) commit only when it
    /// authenticates someone — but the transaction opens on a write-pool
    /// connection. A callback spends its time on outbound HTTP (the code
    /// exchange, the userinfo fetch) and normally touches the database
    /// afterwards, so that round trip holds no connection; one that makes no
    /// CRUD call never takes one.
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition, function resolution, the hook
    /// call, or the commit fails.
    pub fn run_auth_callback(
        &self,
        authenticate: &HookRef,
        input: &AuthStrategyInput,
        infra: &AppInfra,
    ) -> Result<Option<Document>> {
        let hook_tx = AuthHookTx {
            tx: LazyTx::on_pool(infra.pool.clone(), "auth-callback"),
            infra,
        };

        self.run_in_auth_tx(
            hook_tx,
            |lua| call_auth_strategy(lua, authenticate, input),
            Option::is_some,
        )
    }

    /// Run a `password_login` method's `mfa_when` gate: decides whether THIS
    /// verified login must complete a second factor. Lua truthiness applies —
    /// `false`/`nil` skips MFA, anything else requires it (so
    /// `return ctx.user.mfa_enabled` works without a boolean cast). Errors
    /// propagate; the caller fails CLOSED (requires MFA).
    ///
    /// The gate is a predicate, and it runs on whatever connection the
    /// caller is on — including the read connection of every request an
    /// MFA-unstamped session authenticates — so its CRUD is **read-only**:
    /// reads work, every write is refused with an error naming the gate.
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition or the hook call fails.
    pub fn run_mfa_when(
        &self,
        hook: &HookRef,
        input: &MfaWhenInput,
        conn: &dyn DbConnection,
    ) -> Result<bool> {
        let lua = self.pool.acquire()?;

        let _guard = TxContextGuard::set(&lua, conn, None, None, None);
        let _read_only = ReadOnlyScopeGuard::install(&lua, "the `mfa_when` gate");

        let func = resolve_hook_function(&lua, hook.reference())?;

        let ctx = MfaWhenContext {
            collection: input.collection,
            user: &input.user.fields,
            surface: input.surface,
            headers: input.headers,
            options: hook.options(),
        };
        let ctx_value = to_lua_value(&lua, &ctx)?;

        let result: Value = func.call(ctx_value)?;

        Ok(!matches!(result, Value::Boolean(false) | Value::Nil))
    }

    /// Run a `password_login` method's `mfa_deliver` hook (`mfa = "custom"`):
    /// hand the freshly stored code to userland for delivery (SMS, push, …).
    /// The return value is ignored; errors propagate for the caller to log —
    /// delivery is best-effort, like the built-in email path.
    ///
    /// The code is already stored when this runs, so the hook holds no
    /// database connection across its delivery I/O: its CRUD opens one write
    /// transaction lazily, at its first call, committed when the hook returns
    /// and rolled back when it errors.
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition, the hook call, or the commit fails.
    pub fn run_mfa_deliver(
        &self,
        hook: &HookRef,
        input: &MfaDeliverInput,
        infra: &AppInfra,
    ) -> Result<()> {
        let hook_tx = AuthHookTx {
            tx: LazyTx::on_pool(infra.pool.clone(), "mfa_deliver"),
            infra,
        };

        self.run_in_auth_tx(
            hook_tx,
            |lua| call_mfa_deliver(lua, hook, input),
            |(): &()| true,
        )
    }
}
