//! Newtype wrapper for JWT signing secrets with redacted debug output.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A JWT signing secret. Debug output is redacted to prevent accidental logging.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct JwtSecret(String);

impl JwtSecret {
    /// Create a new `JwtSecret` from any string-like value.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Consume the wrapper and return the inner `String`.
    pub fn into_inner(self) -> String {
        self.0
    }

    /// Returns `true` if the secret is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the length of the secret in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for JwtSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JwtSecret([REDACTED])")
    }
}

impl AsRef<str> for JwtSecret {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<[u8]> for JwtSecret {
    fn as_ref(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl From<String> for JwtSecret {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for JwtSecret {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<JwtSecret> for String {
    fn from(s: JwtSecret) -> Self {
        s.0
    }
}

impl Serialize for JwtSecret {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("[REDACTED]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let secret = JwtSecret::new("super-secret-key");
        let debug = format!("{secret:?}");
        assert_eq!(debug, "JwtSecret([REDACTED])");
        assert!(!debug.contains("super-secret-key"));
    }

    #[test]
    fn as_ref_str() {
        let secret = JwtSecret::new("my-secret");
        let s: &str = secret.as_ref();
        assert_eq!(s, "my-secret");
    }

    #[test]
    fn as_ref_bytes() {
        let secret = JwtSecret::new("my-secret");
        let b: &[u8] = secret.as_ref();
        assert_eq!(b, b"my-secret");
    }

    #[test]
    fn from_string() {
        let secret: JwtSecret = "test".to_string().into();
        assert_eq!(secret.as_ref() as &str, "test");
    }

    #[test]
    fn into_inner() {
        let secret = JwtSecret::new("key");
        assert_eq!(secret.into_inner(), "key");
    }

    #[test]
    fn is_empty() {
        assert!(JwtSecret::new("").is_empty());
        assert!(!JwtSecret::new("secret").is_empty());
    }

    #[test]
    fn serialize_is_redacted() {
        let secret = JwtSecret::new("super-secret");
        let json = serde_json::to_string(&secret).unwrap();
        assert_eq!(json, "\"[REDACTED]\"");
        assert!(!json.contains("super-secret"));
    }

    #[test]
    fn deserialize_transparent() {
        let json = "\"my-secret-key\"";
        let secret: JwtSecret = serde_json::from_str(json).unwrap();
        assert_eq!(secret.as_ref() as &str, "my-secret-key");
    }

    #[test]
    fn clone() {
        let secret = JwtSecret::new("key");
        let cloned = secret.clone();
        assert_eq!(cloned.as_ref() as &str, "key");
    }
}
