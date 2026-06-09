//! A hook reference: a dotted Lua function ref plus optional per-config
//! `options` exposed to the hook as `ctx.options`.

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, MapAccess, Visitor},
    ser::SerializeStruct,
};
use serde_json::Value;

use std::fmt;

use crate::typegen::lua::LuaAnnotation;

/// A reference to a Lua hook function, optionally carrying per-configuration
/// `options`.
///
/// One hook function can be reused across collections/fields with different
/// `options`; the resolved options table is exposed to the hook as
/// `ctx.options` (nil for a bare-string ref).
///
/// Both Lua config and serde accept two interchangeable shapes:
/// - a bare string `"hooks.posts.slugify"` (no options), and
/// - a table/map `{ ref = "hooks.posts.slugify", options = { ... } }`.
///
/// Serialization mirrors that: a ref without options serializes as a bare
/// string, so existing registry snapshots (which stored bare strings) round-trip
/// unchanged.
#[derive(Debug, Clone, PartialEq, LuaAnnotation)]
#[lua(class = "crap.HookRef")]
pub struct HookRef {
    /// Dotted Lua function reference, e.g. `"hooks.posts.slugify"`.
    #[lua(rename = "ref")]
    pub reference: String,
    /// Per-config options passed to the hook as `ctx.options` (nil when absent).
    #[lua(ty = "table", optional)]
    pub options: Option<Value>,
}

impl HookRef {
    /// Create a bare hook ref with no options.
    #[must_use]
    pub fn new(reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
            options: None,
        }
    }

    /// Create a hook ref carrying per-config options.
    #[must_use]
    pub fn with_options(reference: impl Into<String>, options: Value) -> Self {
        Self {
            reference: reference.into(),
            options: Some(options),
        }
    }

    /// The dotted function reference.
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// The per-config options, if any.
    #[must_use]
    pub fn options(&self) -> Option<&Value> {
        self.options.as_ref()
    }
}

impl From<String> for HookRef {
    fn from(reference: String) -> Self {
        Self {
            reference,
            options: None,
        }
    }
}

impl From<&str> for HookRef {
    fn from(reference: &str) -> Self {
        Self::new(reference)
    }
}

impl Serialize for HookRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Bare ref → bare string, keeping legacy snapshots stable.
        let Some(options) = &self.options else {
            return serializer.serialize_str(&self.reference);
        };

        let mut s = serializer.serialize_struct("HookRef", 2)?;
        s.serialize_field("ref", &self.reference)?;
        s.serialize_field("options", options)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for HookRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(HookRefVisitor)
    }
}

struct HookRefVisitor;

impl<'de> Visitor<'de> for HookRefVisitor {
    type Value = HookRef;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a hook ref string or a { ref, options } map")
    }

    fn visit_str<E>(self, v: &str) -> Result<HookRef, E>
    where
        E: de::Error,
    {
        Ok(HookRef::new(v))
    }

    fn visit_string<E>(self, v: String) -> Result<HookRef, E>
    where
        E: de::Error,
    {
        Ok(HookRef::from(v))
    }

    fn visit_map<M>(self, mut map: M) -> Result<HookRef, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut reference: Option<String> = None;
        let mut options: Option<Value> = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "ref" => reference = Some(map.next_value()?),
                "options" => options = Some(map.next_value()?),
                other => return Err(de::Error::unknown_field(other, &["ref", "options"])),
            }
        }

        let reference = reference.ok_or_else(|| de::Error::missing_field("ref"))?;

        Ok(HookRef { reference, options })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn bare_ref_serializes_as_string() {
        let r = HookRef::new("hooks.posts.slugify");
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            json!("hooks.posts.slugify")
        );
    }

    #[test]
    fn ref_with_options_serializes_as_map() {
        let r = HookRef::with_options("hooks.posts.slugify", json!({ "from": "title" }));
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            json!({ "ref": "hooks.posts.slugify", "options": { "from": "title" } })
        );
    }

    #[test]
    fn deserializes_from_bare_string() {
        let r: HookRef = serde_json::from_value(json!("hooks.x")).unwrap();
        assert_eq!(r, HookRef::new("hooks.x"));
    }

    #[test]
    fn deserializes_from_map() {
        let r: HookRef =
            serde_json::from_value(json!({ "ref": "hooks.x", "options": { "n": 1 } })).unwrap();
        assert_eq!(r, HookRef::with_options("hooks.x", json!({ "n": 1 })));
    }

    #[test]
    fn map_missing_ref_is_rejected() {
        let err = serde_json::from_value::<HookRef>(json!({ "options": {} })).unwrap_err();
        assert!(err.to_string().contains("ref"));
    }

    #[test]
    fn map_unknown_key_is_rejected() {
        let err =
            serde_json::from_value::<HookRef>(json!({ "ref": "hooks.x", "bogus": 1 })).unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn round_trips_both_shapes() {
        for r in [
            HookRef::new("hooks.a"),
            HookRef::with_options("hooks.b", json!({ "k": [1, 2, 3] })),
        ] {
            let v = serde_json::to_value(&r).unwrap();
            let back: HookRef = serde_json::from_value(v).unwrap();
            assert_eq!(r, back);
        }
    }
}
