//! Parsing functions for collection auth configuration.

use mlua::{Table, Value};

use crate::core::collection::{AuthStrategy, CollectionAuth};

use super::helpers::*;

pub(super) fn parse_collection_auth(config: &Table) -> Option<CollectionAuth> {
    let val: Value = config.get("auth").ok()?;
    match val {
        Value::Boolean(true) => Some(CollectionAuth::new(true)),
        Value::Boolean(false) | Value::Nil => None,
        Value::Table(tbl) => {
            let token_expiry = tbl.get::<u64>("token_expiry").unwrap_or(7200);
            let disable_local = get_bool(&tbl, "disable_local", false);
            let verify_email = get_bool(&tbl, "verify_email", false);
            let forgot_password = get_bool(&tbl, "forgot_password", true);
            let strategies = parse_auth_strategies(&tbl);
            let mut auth = CollectionAuth::new(true);
            auth.token_expiry = token_expiry;
            auth.strategies = strategies;
            auth.disable_local = disable_local;
            auth.verify_email = verify_email;
            auth.forgot_password = forgot_password;
            Some(auth)
        }
        _ => None,
    }
}

fn parse_auth_strategies(tbl: &Table) -> Vec<AuthStrategy> {
    let strategies_tbl = match get_table(tbl, "strategies") {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut strategies = Vec::new();
    for strat_tbl in strategies_tbl.sequence_values::<Table>().flatten() {
        if let (Some(name), Some(authenticate)) = (
            get_string(&strat_tbl, "name"),
            get_string(&strat_tbl, "authenticate"),
        ) {
            strategies.push(AuthStrategy::new(name, authenticate));
        }
    }
    strategies
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn test_parse_collection_auth_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("auth", true).unwrap();
        let auth = parse_collection_auth(&tbl);
        assert!(auth.is_some());
        let auth = auth.unwrap();
        assert!(auth.enabled);
        assert_eq!(auth.token_expiry, 7200);
        assert!(!auth.disable_local);
        assert!(!auth.verify_email);
    }

    #[test]
    fn test_parse_collection_auth_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("auth", false).unwrap();
        assert!(parse_collection_auth(&tbl).is_none());
    }

    #[test]
    fn test_parse_collection_auth_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        auth_tbl.set("token_expiry", 3600u64).unwrap();
        auth_tbl.set("disable_local", true).unwrap();
        auth_tbl.set("verify_email", true).unwrap();
        auth_tbl.set("forgot_password", false).unwrap();
        tbl.set("auth", auth_tbl).unwrap();
        let auth = parse_collection_auth(&tbl);
        assert!(auth.is_some());
        let auth = auth.unwrap();
        assert!(auth.enabled);
        assert_eq!(auth.token_expiry, 3600);
        assert!(auth.disable_local);
        assert!(auth.verify_email);
        assert!(!auth.forgot_password);
    }

    #[test]
    fn test_parse_collection_auth_with_strategies() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let strats = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "oauth").unwrap();
        s1.set("authenticate", "hooks.auth.oauth_check").unwrap();
        strats.set(1, s1).unwrap();
        auth_tbl.set("strategies", strats).unwrap();
        tbl.set("auth", auth_tbl).unwrap();
        let auth = parse_collection_auth(&tbl).unwrap();
        assert_eq!(auth.strategies.len(), 1);
        assert_eq!(auth.strategies[0].name, "oauth");
        assert_eq!(auth.strategies[0].authenticate, "hooks.auth.oauth_check");
    }

    #[test]
    fn test_parse_collection_auth_other_value_returns_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let func = lua.create_function(|_, ()| Ok(())).unwrap();
        tbl.set("auth", func).unwrap();
        assert!(parse_collection_auth(&tbl).is_none());
    }

    #[test]
    fn test_parse_auth_strategies_incomplete_strategy_skipped() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let strats = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "incomplete").unwrap();
        strats.set(1, s1).unwrap();
        let s2 = lua.create_table().unwrap();
        s2.set("name", "oauth").unwrap();
        s2.set("authenticate", "hooks.auth.oauth").unwrap();
        strats.set(2, s2).unwrap();
        auth_tbl.set("strategies", strats).unwrap();
        tbl.set("auth", auth_tbl).unwrap();
        let auth = parse_collection_auth(&tbl).unwrap();
        assert_eq!(auth.strategies.len(), 1);
        assert_eq!(auth.strategies[0].name, "oauth");
    }
}
