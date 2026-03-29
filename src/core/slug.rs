//! Newtype wrapper for collection/global/job slugs.

use std::{borrow::Borrow, fmt, ops::Deref};

use serde::{Deserialize, Serialize};

/// A URL-safe identifier for collections, globals, and jobs.
///
/// Wraps a `String` and provides `Deref<Target=str>` for transparent use
/// wherever `&str` is expected.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Slug(String);

impl Slug {
    /// Create a new `Slug` from any string-like value.
    ///
    /// # Safety invariant
    /// Callers should ensure the slug is valid (lowercase ASCII alphanumeric + underscores,
    /// not starting with `_`). Validation happens at config parsing time via `validate_slug()`.
    /// This constructor is intentionally unchecked to keep it lightweight — the slug flows
    /// directly into SQL identifiers, so the config-level validation is the security boundary.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Consume the wrapper and return the inner `String`.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Deref for Slug {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Slug {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for Slug {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<String> for Slug {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Slug {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<Slug> for String {
    fn from(s: Slug) -> Self {
        s.0
    }
}

impl PartialEq<str> for Slug {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Slug {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for Slug {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_deref() {
        let slug = Slug::new("posts");
        assert_eq!(&*slug, "posts");
    }

    #[test]
    fn into_inner() {
        let slug = Slug::new("posts");
        assert_eq!(slug.into_inner(), "posts".to_string());
    }

    #[test]
    fn display() {
        let slug = Slug::new("posts");
        assert_eq!(format!("{slug}"), "posts");
    }

    #[test]
    fn debug() {
        let slug = Slug::new("posts");
        assert_eq!(format!("{slug:?}"), "\"posts\"");
    }

    #[test]
    fn from_string() {
        let slug: Slug = "posts".to_string().into();
        assert_eq!(slug, "posts");
    }

    #[test]
    fn from_str() {
        let slug: Slug = "posts".into();
        assert_eq!(slug, "posts");
    }

    #[test]
    fn into_string() {
        let slug = Slug::new("posts");
        let s: String = slug.into();
        assert_eq!(s, "posts");
    }

    #[test]
    fn partial_eq_str() {
        let slug = Slug::new("posts");
        assert!(slug == "posts");
        assert!(slug != "pages");
    }

    #[test]
    fn partial_eq_string() {
        let slug = Slug::new("posts");
        assert!(slug == "posts");
    }

    #[test]
    fn hash_map_borrow_lookup() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert(Slug::new("posts"), 1);
        assert_eq!(map.get("posts"), Some(&1));
    }

    #[test]
    fn serde_roundtrip() {
        let slug = Slug::new("posts");
        let json = serde_json::to_string(&slug).unwrap();
        assert_eq!(json, "\"posts\"");
        let deserialized: Slug = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, slug);
    }

    #[test]
    fn str_methods_via_deref() {
        let slug = Slug::new("my_posts");
        assert!(slug.starts_with("my"));
        assert_eq!(slug.len(), 8);
        assert!(slug.contains("post"));
    }

    #[test]
    fn clone() {
        let slug = Slug::new("posts");
        let cloned = slug.clone();
        assert_eq!(slug, cloned);
    }

    #[test]
    fn ord() {
        let a = Slug::new("alpha");
        let b = Slug::new("beta");
        assert!(a < b);
    }
}
