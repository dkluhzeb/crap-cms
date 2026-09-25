//! Request bodies as Lua holds them: byte strings, UTF-8 or not.
//!
//! A Lua string is a byte string, and `crap.http` carries binary payloads
//! (file uploads to a custom storage backend, images, archives). Deserializing
//! the body into a Rust `String` refused every non-UTF-8 byte, so the body is
//! taken as raw bytes instead.

use std::fmt::{Formatter, Result as FmtResult};

use serde::{
    Deserialize, Deserializer,
    de::{Error as DeError, Visitor},
};

/// A request body: the Lua string's bytes, unchanged.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LuaBody(pub(crate) Vec<u8>);

/// Accepts a string in either form a deserializer hands one over.
struct BodyVisitor;

impl Visitor<'_> for BodyVisitor {
    type Value = LuaBody;

    fn expecting(&self, f: &mut Formatter) -> FmtResult {
        f.write_str("a string")
    }

    fn visit_str<E: DeError>(self, v: &str) -> Result<LuaBody, E> {
        Ok(LuaBody(v.as_bytes().to_vec()))
    }

    fn visit_bytes<E: DeError>(self, v: &[u8]) -> Result<LuaBody, E> {
        Ok(LuaBody(v.to_vec()))
    }

    fn visit_byte_buf<E: DeError>(self, v: Vec<u8>) -> Result<LuaBody, E> {
        Ok(LuaBody(v))
    }
}

impl<'de> Deserialize<'de> for LuaBody {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_byte_buf(BodyVisitor)
    }
}

#[cfg(test)]
mod tests {
    use mlua::{Lua, LuaSerdeExt as _};

    use super::*;

    #[test]
    fn a_binary_lua_string_keeps_its_bytes() {
        let lua = Lua::new();
        let value = lua.load(r#"return "\0\255\128png""#).eval().unwrap();

        let body: LuaBody = lua.from_value(value).unwrap();

        assert_eq!(body.0, b"\0\xff\x80png");
    }

    #[test]
    fn a_text_lua_string_is_its_utf8_bytes() {
        let lua = Lua::new();
        let value = lua.load(r#"return "héllo""#).eval().unwrap();

        let body: LuaBody = lua.from_value(value).unwrap();

        assert_eq!(body.0, "héllo".as_bytes());
    }

    #[test]
    fn a_non_string_is_refused() {
        let lua = Lua::new();
        let value = lua.load("return {}").eval().unwrap();

        assert!(lua.from_value::<LuaBody>(value).is_err());
    }
}
