//! Helper for retrieving the active transaction connection from Lua `app_data`.
//! Used by every Lua CRUD closure to access the shared transaction.

use mlua::{Error::RuntimeError, Lua, Result as LuaResult};

use crate::{db::DbConnection, hooks::lifecycle::TxContext};

/// Get the active transaction connection from Lua `app_data`.
/// Returns an error if called outside of `run_hooks_with_conn`.
///
/// The returned reference is valid for the duration of the current hook call.
/// `TxContextGuard` (set by the runner) keeps the underlying connection alive
/// until the hook returns, and the `&Lua` borrow forces callers to release the
/// reference before the VM can be reused for another call.
pub(crate) fn get_tx_conn(lua: &Lua) -> LuaResult<&dyn DbConnection> {
    let ctx = lua.app_data_ref::<TxContext>().ok_or_else(|| {
        RuntimeError(
            "crap.collections CRUD functions are only available inside hooks \
             with transaction context (before_change, before_delete, etc.)"
                .into(),
        )
    })?;
    let ptr = ctx.as_ptr();
    // SAFETY: `TxContextGuard` (constructed in `run_hooks_with_conn` and
    // friends) holds the connection borrow for the full duration of this hook
    // call. The guard removes the `TxContext` from app_data on drop, which
    // strictly outlives any `&'a dyn DbConnection` we hand out tied to `&'a Lua`.
    Ok(unsafe { &*ptr })
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn test_get_tx_conn_without_context() {
        let lua = Lua::new();
        let Err(err) = get_tx_conn(&lua) else {
            panic!("expected error when called outside hook context");
        };
        assert!(err.to_string().contains("only available inside hooks"));
    }
}
