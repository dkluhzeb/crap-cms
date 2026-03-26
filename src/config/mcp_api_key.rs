//! Newtype wrapper for MCP API keys with redacted debug output.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An MCP API key. Debug output is redacted to prevent accidental logging.
#[derive(Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct McpApiKey(String);

impl McpApiKey {
    /// Create a new `McpApiKey` from a string.
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// Returns `true` if the API key is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&str> for McpApiKey {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl fmt::Debug for McpApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("McpApiKey([REDACTED])")
    }
}

impl fmt::Display for McpApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for McpApiKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for McpApiKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("[REDACTED]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let key = McpApiKey("my-secret-key".to_string());
        let debug = format!("{key:?}");
        assert_eq!(debug, "McpApiKey([REDACTED])");
        assert!(!debug.contains("my-secret-key"));
    }

    #[test]
    fn as_ref_str() {
        let key = McpApiKey("key123".to_string());
        let s: &str = key.as_ref();
        assert_eq!(s, "key123");
    }

    #[test]
    fn is_empty() {
        assert!(McpApiKey::default().is_empty());
        assert!(!McpApiKey("key".to_string()).is_empty());
    }

    #[test]
    fn serialize_is_redacted() {
        let key = McpApiKey("super-secret".to_string());
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, "\"[REDACTED]\"");
        assert!(!json.contains("super-secret"));
    }

    #[test]
    fn deserialize_transparent() {
        let json = "\"my-key\"";
        let key: McpApiKey = serde_json::from_str(json).unwrap();
        assert_eq!(key.as_ref(), "my-key");
    }
}
