//! `HookRunner` methods for the auth hooks: custom strategies, auth
//! callbacks, and the `password_login` method's `mfa_when` / `mfa_deliver`.
//!
//! Each hook's CRUD contract follows what it is for:
//!
//! - A **strategy** (at login, or per request) runs in a transaction on the
//!   caller's connection that commits only when it authenticates someone.
//! - An **auth callback** and an **`mfa_deliver`** hook run their outbound
//!   HTTP without a database connection: their transaction is opened lazily
//!   on a write connection at their first CRUD call ([`LazyTx`]), so the
//!   network round trip never pins a write connection unless the hook
//!   touched the database before it.
//! - **`mfa_when`** is a read-only predicate on the caller's connection.

use anyhow::{Context as _, Result};
use mlua::{Lua, Table, Value};

use crate::{
    core::{Document, HookRef, document::DocumentBuilder},
    db::{DbConnection, DbPool},
    hooks::{
        HookRunner,
        lifecycle::{
            AuthStrategyContext, AuthStrategyInput, LazyTx, LazyTxGuard, MfaDeliverContext,
            MfaDeliverInput, MfaWhenContext, MfaWhenInput, ReadOnlyScopeGuard, TxContextGuard,
            commit_or_roll_back, converters::lua_table_to_json_map,
            execution::resolve_hook_function, roll_back,
        },
        lua_api::to_lua_value,
    },
};

/// Transaction label of a strategy run on the caller's connection.
const STRATEGY_TX: &str = "auth-strategy";

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
    /// Run a custom auth strategy function on `conn` (a login's connection,
    /// or the request's own connection for a per-request strategy). Returns
    /// the user it authenticates, if any.
    ///
    /// The strategy runs inside a transaction that COMMITS only when it
    /// authenticates someone. A failed or erroring attempt rolls back —
    /// strategy attempts are attacker-controlled (unauthenticated input), so
    /// persistent writes keyed to failures would let anyone grow the database
    /// from the login endpoint. Counters belong in the rate limiters,
    /// observability in `crap.log` (neither lives in this transaction).
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
    ) -> Result<Option<Document>> {
        let lua = self.pool.acquire()?;

        // Inject connection for CRUD access — guard ensures cleanup on all exit paths
        let _guard = TxContextGuard::set(&lua, conn, None, None, None);

        conn.execute("BEGIN", &[])
            .context("failed to open the auth-strategy transaction")?;

        let outcome = call_auth_strategy(&lua, authenticate, input);

        if matches!(outcome, Ok(Some(_))) {
            commit_or_roll_back(conn, STRATEGY_TX)?;
        } else {
            roll_back(conn, STRATEGY_TX);
        }

        outcome
    }

    /// Run an auth-callback hook (OAuth / OIDC: `auth_callback/{name}.lua`)
    /// against `pool`. Returns the user it authenticates, if any.
    ///
    /// Same contract as [`Self::run_auth_strategy`] — its writes (typically
    /// provisioning the user on first sign-in) commit only when it
    /// authenticates someone — but the transaction opens **lazily**, on a
    /// write connection, at the hook's first CRUD call. A callback spends its
    /// time on outbound HTTP (the code exchange, the userinfo fetch) and
    /// normally touches the database afterwards, so that round trip holds no
    /// connection; one that makes no CRUD call never takes one.
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition, function resolution, the hook
    /// call, or the commit fails.
    pub fn run_auth_callback(
        &self,
        authenticate: &HookRef,
        input: &AuthStrategyInput,
        pool: &DbPool,
    ) -> Result<Option<Document>> {
        let lua = self.pool.acquire()?;
        let tx = LazyTx::new(pool.clone(), "auth-callback");

        let outcome = {
            let _identity = TxContextGuard::set_identity(&lua, None, None);
            let _lazy = LazyTxGuard::install(&lua, &tx);

            call_auth_strategy(&lua, authenticate, input)
        };

        // Dropping the transaction without committing rolls it back.
        if matches!(outcome, Ok(Some(_))) {
            tx.commit()?;
        }

        outcome
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
        pool: &DbPool,
    ) -> Result<()> {
        let lua = self.pool.acquire()?;
        let tx = LazyTx::new(pool.clone(), "mfa_deliver");

        {
            let _identity = TxContextGuard::set_identity(&lua, None, None);
            let _lazy = LazyTxGuard::install(&lua, &tx);

            call_mfa_deliver(&lua, hook, input)?;
        }

        tx.commit()
    }
}
