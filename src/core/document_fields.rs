//! Typed view of a document's user-defined fields.
//!
//! Distinct at the type level from [`crate::core::ReqContext`] (the
//! per-request hook scratchpad) and `MutationEvent.data` (the broadcast
//! payload), even though all three are JSON maps. Wrapping the underlying
//! `HashMap<String, Value>` in a newtype keeps these three same-shape maps
//! from being mixed up at API boundaries — `Document.fields`,
//! `WriteInput.data`, and `HookContext.data` all carry user document data
//! and share this type, while `ReqContext` does not.

use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A document's user-defined field values. See module docs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct DocumentFields(HashMap<String, Value>);

impl DocumentFields {
    /// Create an empty field map.
    #[must_use]
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Borrow the inner map.
    #[must_use]
    pub fn as_map(&self) -> &HashMap<String, Value> {
        &self.0
    }

    /// Consume the wrapper and return the inner map.
    #[must_use]
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

    /// Read a key as an f64 if the value is a JSON number.
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.0.get(key).and_then(Value::as_f64)
    }
}

impl Deref for DocumentFields {
    type Target = HashMap<String, Value>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for DocumentFields {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<HashMap<String, Value>> for DocumentFields {
    fn from(m: HashMap<String, Value>) -> Self {
        Self(m)
    }
}

impl From<DocumentFields> for HashMap<String, Value> {
    fn from(f: DocumentFields) -> Self {
        f.0
    }
}

impl IntoIterator for DocumentFields {
    type Item = (String, Value);
    type IntoIter = std::collections::hash_map::IntoIter<String, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a DocumentFields {
    type Item = (&'a String, &'a Value);
    type IntoIter = std::collections::hash_map::Iter<'a, String, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl FromIterator<(String, Value)> for DocumentFields {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        Self(HashMap::from_iter(iter))
    }
}

impl Extend<(String, Value)> for DocumentFields {
    fn extend<T: IntoIterator<Item = (String, Value)>>(&mut self, iter: T) {
        self.0.extend(iter);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn empty_fields_is_empty() {
        let f = DocumentFields::new();
        assert!(f.is_empty());
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn deref_allows_hashmap_methods() {
        let mut f = DocumentFields::new();
        f.insert("k".into(), json!("v"));
        assert_eq!(f.get("k"), Some(&json!("v")));
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn typed_accessors() {
        let mut f = DocumentFields::new();
        f.insert("name".into(), json!("alice"));
        f.insert("active".into(), json!(true));
        f.insert("count".into(), json!(42));
        f.insert("ratio".into(), json!(1.5));

        assert_eq!(f.get_str("name"), Some("alice"));
        assert_eq!(f.get_bool("active"), Some(true));
        assert_eq!(f.get_i64("count"), Some(42));
        assert_eq!(f.get_f64("ratio"), Some(1.5));

        assert_eq!(f.get_str("count"), None);
        assert_eq!(f.get_bool("name"), None);
    }

    #[test]
    fn from_hashmap_and_back() {
        let mut m = HashMap::new();
        m.insert("k".to_string(), json!(1));

        let f: DocumentFields = m.clone().into();
        assert_eq!(f.as_map(), &m);

        let back: HashMap<String, Value> = f.into();
        assert_eq!(back, m);
    }

    #[test]
    fn serializes_transparently() {
        let mut f = DocumentFields::new();
        f.insert("k".into(), json!("v"));

        let s = serde_json::to_string(&f).unwrap();
        assert_eq!(s, r#"{"k":"v"}"#);

        let back: DocumentFields = serde_json::from_str(&s).unwrap();
        assert_eq!(back.get_str("k"), Some("v"));
    }

    #[test]
    fn extend_merges_iterator() {
        let mut f = DocumentFields::new();
        f.insert("a".into(), json!(1));

        f.extend([("b".to_string(), json!(2)), ("c".to_string(), json!(3))]);

        assert_eq!(f.len(), 3);
        assert_eq!(f.get("b"), Some(&json!(2)));
    }

    #[test]
    fn collect_from_iterator() {
        let f: DocumentFields = [("x".to_string(), json!(1)), ("y".to_string(), json!(2))]
            .into_iter()
            .collect();
        assert_eq!(f.len(), 2);
    }
}
