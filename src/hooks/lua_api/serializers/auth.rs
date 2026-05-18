//! Lua table serializer for collection auth configuration.

use mlua::{Lua, Table};

use crate::core::{
    CollectionDefinition,
    collection::{Activation, AuthMethod, MfaMode, Surface, SurfaceSet},
};

/// Serialize the auth section of a `CollectionDefinition` into the Lua table.
pub(super) fn collection_auth_to_lua(
    lua: &Lua,
    tbl: &Table,
    def: &CollectionDefinition,
) -> mlua::Result<()> {
    let Some(ref auth) = def.auth else {
        return Ok(());
    };

    if !auth.enabled {
        return Ok(());
    }

    let auth_tbl = lua.create_table()?;
    auth_tbl.set("enabled", true)?;
    if auth.token_expiry != 7200 {
        auth_tbl.set("token_expiry", auth.token_expiry)?;
    }

    if !auth.methods.is_empty() {
        let methods = lua.create_table()?;
        for (i, m) in auth.methods.iter().enumerate() {
            methods.set(i + 1, method_to_lua(lua, m)?)?;
        }
        auth_tbl.set("methods", methods)?;
    }

    tbl.set("auth", auth_tbl)
}

fn method_to_lua(lua: &Lua, m: &AuthMethod) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    match m {
        AuthMethod::PasswordLogin {
            mfa,
            verify_email,
            forgot_password,
        } => {
            t.set("type", "password_login")?;
            if matches!(mfa, MfaMode::Email) {
                t.set("mfa", "email")?;
            }
            if *verify_email {
                t.set("verify_email", true)?;
            }
            if !*forgot_password {
                t.set("forgot_password", false)?;
            }
        }
        AuthMethod::Bearer { surfaces } => {
            t.set("type", "bearer")?;
            if surfaces != &SurfaceSet::all() {
                t.set("surfaces", surfaces_to_lua(lua, surfaces)?)?;
            }
        }
        AuthMethod::SessionCookie { surfaces } => {
            t.set("type", "session_cookie")?;
            if surfaces != &SurfaceSet::admin_only() {
                t.set("surfaces", surfaces_to_lua(lua, surfaces)?)?;
            }
        }
        AuthMethod::Strategy {
            name,
            authenticate,
            activates_on,
            surfaces,
        } => {
            t.set("type", "strategy")?;
            t.set("name", name.as_str())?;
            t.set("authenticate", authenticate.as_str())?;
            t.set("activates_on", activation_to_lua(lua, activates_on)?)?;
            if surfaces != &SurfaceSet::admin_only() {
                t.set("surfaces", surfaces_to_lua(lua, surfaces)?)?;
            }
        }
    }
    Ok(t)
}

fn surfaces_to_lua(lua: &Lua, surfaces: &SurfaceSet) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (i, s) in surfaces.iter().enumerate() {
        let name = match s {
            Surface::Admin => "admin",
            Surface::Grpc => "grpc",
        };
        t.set(i + 1, name)?;
    }
    Ok(t)
}

fn activation_to_lua(lua: &Lua, act: &Activation) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    match act {
        Activation::Always(_) => {
            t.set("always", true)?;
        }
        Activation::Header { header } => {
            t.set("header", header.as_str())?;
        }
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use crate::core::{
        CollectionDefinition,
        collection::{Activation, Auth, AuthMethod, SurfaceSet},
    };
    use crate::hooks::lua_api::serializers::collection::collection_config_to_lua;
    use mlua::{self, Value};

    #[test]
    fn auth_enabled_with_default_methods() {
        let lua = mlua::Lua::new();
        let mut def = CollectionDefinition::new("users");
        def.timestamps = true;
        def.auth = Some(Auth {
            enabled: true,
            methods: Auth::default_methods(),
            ..Default::default()
        });
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let auth_tbl: mlua::Table = tbl.get("auth").unwrap();
        let methods: mlua::Table = auth_tbl.get("methods").unwrap();
        assert_eq!(methods.len().unwrap(), 3);
    }

    #[test]
    fn auth_with_strategy_method() {
        let lua = mlua::Lua::new();
        let mut def = CollectionDefinition::new("users");
        let auth = Auth {
            enabled: true,
            methods: vec![AuthMethod::Strategy {
                name: "oauth".into(),
                authenticate: "hooks.auth.oauth".into(),
                activates_on: Activation::Header {
                    header: "x-oauth".into(),
                },
                surfaces: SurfaceSet::grpc_only(),
            }],
            ..Default::default()
        };
        def.auth = Some(auth);
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let auth_tbl: mlua::Table = tbl.get("auth").unwrap();
        let methods: mlua::Table = auth_tbl.get("methods").unwrap();
        let m1: mlua::Table = methods.get(1).unwrap();
        assert_eq!(m1.get::<String>("type").unwrap(), "strategy");
        assert_eq!(m1.get::<String>("name").unwrap(), "oauth");
        let act: mlua::Table = m1.get("activates_on").unwrap();
        assert_eq!(act.get::<String>("header").unwrap(), "x-oauth");
    }

    #[test]
    fn auth_disabled_not_emitted() {
        let lua = mlua::Lua::new();
        let mut def = CollectionDefinition::new("items");
        def.timestamps = false;
        def.auth = Some(Auth::new(false));
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let auth_val: Value = tbl.get("auth").unwrap();
        assert!(matches!(auth_val, Value::Nil));
    }
}
