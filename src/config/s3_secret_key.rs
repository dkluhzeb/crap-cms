//! Newtype wrapper for S3 secret access keys with redacted output.

use std::fmt;

use serde::{Deserialize, Serialize, Serializer};

/// An S3 secret access key. `Debug` and `Serialize` are redacted so the
/// secret never leaks via tracing, JSON dumps, or `crap.config.get`
/// from a Lua hook. The other config secrets (`JwtSecret`,
/// `SmtpPassword`, `McpApiKey`) wrap the same way; S3 was previously
/// the only one stored as a bare `String`.
#[derive(Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct S3SecretKey(String);

impl S3SecretKey {
    /// Create a new `S3SecretKey` from any string-like value.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Returns `true` if the secret key is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for S3SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("S3SecretKey([REDACTED])")
    }
}

impl AsRef<str> for S3SecretKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for S3SecretKey {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for S3SecretKey {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl Serialize for S3SecretKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("[REDACTED]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: round-10 audit found `upload.s3.secret_key` was the
    /// only config secret stored as a bare `String`, leaking via
    /// `crap.config.get` from Lua hooks and via any debug/JSON dump of
    /// `CrapConfig`. The wrapper redacts both `Debug` and `Serialize`,
    /// matching the existing `JwtSecret` / `SmtpPassword` / `McpApiKey`
    /// pattern.
    #[test]
    fn debug_is_redacted() {
        let key = S3SecretKey::new("AKIA-very-secret");
        let debug = format!("{key:?}");
        assert_eq!(debug, "S3SecretKey([REDACTED])");
        assert!(!debug.contains("AKIA-very-secret"));
    }

    #[test]
    fn serialize_is_redacted() {
        let key = S3SecretKey::new("super-secret");
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, "\"[REDACTED]\"");
        assert!(!json.contains("super-secret"));
    }

    #[test]
    fn deserialize_transparent() {
        let json = "\"my-key\"";
        let key: S3SecretKey = serde_json::from_str(json).unwrap();
        assert_eq!(key.as_ref(), "my-key");
    }

    #[test]
    fn as_ref_str() {
        let key = S3SecretKey::new("key123");
        let s: &str = key.as_ref();
        assert_eq!(s, "key123");
    }

    #[test]
    fn is_empty() {
        assert!(S3SecretKey::default().is_empty());
        assert!(!S3SecretKey::new("k").is_empty());
    }

    #[test]
    fn from_str_and_string() {
        let from_str: S3SecretKey = "abc".into();
        assert_eq!(from_str.as_ref(), "abc");
        let from_string: S3SecretKey = "def".to_string().into();
        assert_eq!(from_string.as_ref(), "def");
    }
}
