//! Newtype wrapper for Argon2id password hashes with redacted debug output.

use std::fmt;

/// An Argon2id password hash (PHC string). Debug output is redacted to prevent
/// accidental logging.
#[derive(Clone)]
pub struct HashedPassword(String);

impl HashedPassword {
    /// Create a new `HashedPassword` from any string-like value.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl fmt::Debug for HashedPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HashedPassword([REDACTED])")
    }
}

impl AsRef<str> for HashedPassword {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for HashedPassword {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let hash = HashedPassword::new("$argon2id$v=19$...");
        let debug = format!("{hash:?}");
        assert_eq!(debug, "HashedPassword([REDACTED])");
        assert!(!debug.contains("argon2"));
    }

    #[test]
    fn as_ref_str() {
        let hash = HashedPassword::new("$argon2id$v=19$test");
        let s: &str = hash.as_ref();
        assert_eq!(s, "$argon2id$v=19$test");
    }

    #[test]
    fn from_string() {
        let hash: HashedPassword = "$argon2id$v=19$test".to_string().into();
        assert_eq!(hash.as_ref(), "$argon2id$v=19$test");
    }

    #[test]
    fn clone() {
        let hash = HashedPassword::new("hash");
        let cloned = hash.clone();
        assert_eq!(cloned.as_ref(), "hash");
    }
}
