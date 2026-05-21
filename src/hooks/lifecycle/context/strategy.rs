//! Context types passed to auth/access hook callbacks. Each struct
//! carries `Serialize` (so the hook runner builds it once and Lua sees
//! it via `lua.to_value()`) and `LuaAnnotation` (so the Lua-side type
//! lives next to the Rust struct that produces it).

use std::collections::HashMap;

use serde::Serialize;

use crate::typegen::lua::LuaAnnotation;

/// Context passed to `strategy`-type auth `authenticate` hooks.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.AuthStrategyContext")]
pub struct AuthStrategyContext<'a> {
    /// Request headers (lowercase keys).
    #[lua(ty = "table<string, string>")]
    pub headers: &'a HashMap<String, String>,
    /// Auth collection slug.
    pub collection: &'a str,
}
