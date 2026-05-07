//! Request-scoped context bag that flows through the hook lifecycle.
//!
//! Distinct from `Document.fields` (the actual content of a document) and
//! `MutationEvent.data` (the broadcast payload). This is a per-request
//! scratchpad that hooks read and write to share state across the
//! `before_validate` → `before_change` → `after_change` pipeline (and the
//! corresponding read/delete/unpublish chains).
//!
//! Keys are user-defined; values are JSON-compatible. Nothing in the Rust
//! core inspects the contents — it is transit between Lua hook invocations.
//! Wrapping `HashMap<String, Value>` in a newtype keeps it from being
//! confused with the other two same-shape maps at the type level.

use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Per-request hook-context bag. See module docs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReqContext(HashMap<String, Value>);

impl ReqContext {
    /// Create an empty context.
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Borrow the inner map.
    pub fn as_map(&self) -> &HashMap<String, Value> {
        &self.0
    }

    /// Consume the wrapper and return the inner map.
    pub fn into_inner(self) -> HashMap<String, Value> {
        self.0
    }

    /// Read a key as a borrowed string if the value is a JSON string.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }

    /// Read a key as a bool if the value is a JSON bool.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.0.get(key).and_then(Value::as_bool)
    }

    /// Read a key as an i64 if the value is a JSON integer.
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.0.get(key).and_then(Value::as_i64)
    }
}

impl Deref for ReqContext {
    type Target = HashMap<String, Value>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ReqContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<HashMap<String, Value>> for ReqContext {
    fn from(m: HashMap<String, Value>) -> Self {
        Self(m)
    }
}

impl From<ReqContext> for HashMap<String, Value> {
    fn from(c: ReqContext) -> Self {
        c.0
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn empty_context_is_empty() {
        let ctx = ReqContext::new();
        assert!(ctx.is_empty());
        assert_eq!(ctx.len(), 0);
    }

    #[test]
    fn deref_allows_hashmap_methods() {
        let mut ctx = ReqContext::new();
        ctx.insert("k".into(), json!("v"));
        assert_eq!(ctx.get("k"), Some(&json!("v")));
        assert_eq!(ctx.len(), 1);
    }

    #[test]
    fn typed_accessors() {
        let mut ctx = ReqContext::new();
        ctx.insert("name".into(), json!("alice"));
        ctx.insert("active".into(), json!(true));
        ctx.insert("count".into(), json!(42));

        assert_eq!(ctx.get_str("name"), Some("alice"));
        assert_eq!(ctx.get_bool("active"), Some(true));
        assert_eq!(ctx.get_i64("count"), Some(42));

        // Wrong-typed reads return None instead of panicking.
        assert_eq!(ctx.get_str("count"), None);
        assert_eq!(ctx.get_bool("name"), None);
    }

    #[test]
    fn from_hashmap_and_back() {
        let mut m = HashMap::new();
        m.insert("k".to_string(), json!(1));

        let ctx: ReqContext = m.clone().into();
        assert_eq!(ctx.as_map(), &m);

        let back: HashMap<String, Value> = ctx.into();
        assert_eq!(back, m);
    }

    #[test]
    fn serializes_transparently() {
        let mut ctx = ReqContext::new();
        ctx.insert("k".into(), json!("v"));

        let s = serde_json::to_string(&ctx).unwrap();
        assert_eq!(s, r#"{"k":"v"}"#);

        let back: ReqContext = serde_json::from_str(&s).unwrap();
        assert_eq!(back.get_str("k"), Some("v"));
    }
}
