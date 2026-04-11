//! Newtype wrapper for document primary keys (nanoid).

use std::{borrow::Borrow, fmt, ops::Deref};

use serde::{Deserialize, Serialize};

/// A document primary key, typically a nanoid.
///
/// Wraps a `String` and provides `Deref<Target=str>` for transparent use
/// wherever `&str` is expected.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocumentId(String);

impl DocumentId {
    /// Create a new `DocumentId` from any string-like value.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Consume the wrapper and return the inner `String`.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Deref for DocumentId {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for DocumentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for DocumentId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<String> for DocumentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for DocumentId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<DocumentId> for String {
    fn from(id: DocumentId) -> Self {
        id.0
    }
}

impl PartialEq<str> for DocumentId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for DocumentId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for DocumentId {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_and_traits() {
        let id = DocumentId::new("abc123");

        // Deref, Display, Debug
        assert_eq!(&*id, "abc123");
        assert_eq!(format!("{id}"), "abc123");
        assert_eq!(format!("{id:?}"), "\"abc123\"");

        // Clone
        assert_eq!(id.clone(), "abc123");

        // Into/From conversions
        assert_eq!(id.into_inner(), "abc123");
        assert_eq!(<DocumentId as From<&str>>::from("x"), "x");
        assert_eq!(<DocumentId as From<String>>::from("y".into()), "y");
        assert_eq!(String::from(DocumentId::new("z")), "z");
    }

    #[test]
    fn partial_eq_variants() {
        let id = DocumentId::new("abc123");

        assert!(id == "abc123");
        assert!(id != "xyz789");

        let owned = "abc123".to_string();
        assert!(id == owned);
    }

    #[test]
    fn serde_roundtrip() {
        let id = DocumentId::new("abc123");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"abc123\"");

        let deserialized: DocumentId = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, id);
    }
}
