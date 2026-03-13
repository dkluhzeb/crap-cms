//! Lua table serializer for collection auth configuration.

use mlua::{Lua, Table};

use crate::core::CollectionDefinition;

/// Serialize the auth section of a CollectionDefinition into the Lua table.
pub(super) fn collection_auth_to_lua(
    lua: &Lua,
    tbl: &Table,
    def: &CollectionDefinition,
) -> mlua::Result<()> {
    if let Some(ref auth) = def.auth
        && auth.enabled
    {
        if auth.strategies.is_empty()
            && !auth.disable_local
            && !auth.verify_email
            && auth.forgot_password
            && auth.token_expiry == 7200
        {
            tbl.set("auth", true)?;
        } else {
            let auth_tbl = lua.create_table()?;
            auth_tbl.set("token_expiry", auth.token_expiry)?;

            if auth.disable_local {
                auth_tbl.set("disable_local", true)?;
            }
            if auth.verify_email {
                auth_tbl.set("verify_email", true)?;
            }
            if !auth.forgot_password {
                auth_tbl.set("forgot_password", false)?;
            }
            if !auth.strategies.is_empty() {
                let strats = lua.create_table()?;
                for (i, s) in auth.strategies.iter().enumerate() {
                    let st = lua.create_table()?;
                    st.set("name", s.name.as_str())?;
                    st.set("authenticate", s.authenticate.as_str())?;
                    strats.set(i + 1, st)?;
                }
                auth_tbl.set("strategies", strats)?;
            }
            tbl.set("auth", auth_tbl)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::collection::collection_config_to_lua;
    use crate::core::{
        CollectionDefinition,
        collection::{Auth, AuthStrategy},
    };
    use mlua::{self, Value};

    #[test]
    fn test_collection_config_to_lua_with_auth_simple() {
        let lua = mlua::Lua::new();
        let mut def = CollectionDefinition::new("users");
        def.timestamps = true;
        def.auth = Some(Auth::new(true));
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let auth_val: bool = tbl.get("auth").unwrap();
        assert!(auth_val);
    }

    #[test]
    fn test_collection_config_to_lua_with_auth_complex() {
        let lua = mlua::Lua::new();
        let mut def = CollectionDefinition::new("users");
        def.timestamps = true;
        let mut auth = Auth::new(true);
        auth.token_expiry = 3600;
        auth.disable_local = true;
        auth.verify_email = true;
        auth.forgot_password = false;
        auth.strategies = vec![AuthStrategy::new("oauth", "hooks.auth.oauth")];
        def.auth = Some(auth);
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let auth_tbl: mlua::Table = tbl.get("auth").unwrap();
        assert_eq!(auth_tbl.get::<u64>("token_expiry").unwrap(), 3600);
        assert_eq!(auth_tbl.get::<bool>("disable_local").unwrap(), true);
        assert_eq!(auth_tbl.get::<bool>("verify_email").unwrap(), true);
        assert_eq!(auth_tbl.get::<bool>("forgot_password").unwrap(), false);
        let strats: mlua::Table = auth_tbl.get("strategies").unwrap();
        let s1: mlua::Table = strats.get(1).unwrap();
        let sname: String = s1.get("name").unwrap();
        assert_eq!(sname, "oauth");
    }

    #[test]
    fn test_collection_config_to_lua_auth_disabled_not_emitted() {
        let lua = mlua::Lua::new();
        let mut def = CollectionDefinition::new("items");
        def.timestamps = false;
        def.auth = Some(Auth::new(false));
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let auth_val: Value = tbl.get("auth").unwrap();
        assert!(matches!(auth_val, Value::Nil), "auth = None when disabled");
    }
}
