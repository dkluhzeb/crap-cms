//! Newtype wrapper for SMTP passwords with redacted debug output.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An SMTP password. Debug output is redacted to prevent accidental logging.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct SmtpPassword(String);

impl SmtpPassword {
    /// Create a new `SmtpPassword` from any string-like value.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Returns `true` if the password is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SmtpPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SmtpPassword([REDACTED])")
    }
}

impl AsRef<str> for SmtpPassword {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for SmtpPassword {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for SmtpPassword {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<SmtpPassword> for String {
    fn from(s: SmtpPassword) -> Self {
        s.0
    }
}

impl Serialize for SmtpPassword {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("[REDACTED]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let pass = SmtpPassword::new("my-smtp-pass");
        let debug = format!("{pass:?}");
        assert_eq!(debug, "SmtpPassword([REDACTED])");
        assert!(!debug.contains("my-smtp-pass"));
    }

    #[test]
    fn as_ref_str() {
        let pass = SmtpPassword::new("pass123");
        let s: &str = pass.as_ref();
        assert_eq!(s, "pass123");
    }

    #[test]
    fn from_string() {
        let pass: SmtpPassword = "test".to_string().into();
        assert_eq!(pass.as_ref(), "test");
    }

    #[test]
    fn is_empty() {
        assert!(SmtpPassword::new("").is_empty());
        assert!(!SmtpPassword::new("pass").is_empty());
    }

    #[test]
    fn serialize_is_redacted() {
        let pass = SmtpPassword::new("super-secret");
        let json = serde_json::to_string(&pass).unwrap();
        assert_eq!(json, "\"[REDACTED]\"");
        assert!(!json.contains("super-secret"));
    }

    #[test]
    fn deserialize_transparent() {
        let json = "\"my-pass\"";
        let pass: SmtpPassword = serde_json::from_str(json).unwrap();
        assert_eq!(pass.as_ref(), "my-pass");
    }

    #[test]
    fn clone() {
        let pass = SmtpPassword::new("key");
        let cloned = pass.clone();
        assert_eq!(cloned.as_ref(), "key");
    }
}
