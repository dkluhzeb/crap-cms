//! Helper for retrieving the active transaction connection from Lua app_data.
//! Used by every Lua CRUD closure to access the shared transaction.

use mlua::{Error::RuntimeError, Lua, Result as LuaResult};

use crate::{db::DbConnection, hooks::lifecycle::TxContext};

/// Get the active transaction connection from Lua app_data.
/// Returns an error if called outside of `run_hooks_with_conn`.
pub(crate) fn get_tx_conn(lua: &Lua) -> LuaResult<*const dyn DbConnection> {
    let ctx = lua.app_data_ref::<TxContext>().ok_or_else(|| {
        RuntimeError(
            "crap.collections CRUD functions are only available inside hooks \
             with transaction context (before_change, before_delete, etc.)"
                .into(),
        )
    })?;
    Ok(ctx.as_ptr())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn test_get_tx_conn_without_context() {
        let lua = Lua::new();
        let result = get_tx_conn(&lua);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("only available inside hooks")
        );
    }
}
