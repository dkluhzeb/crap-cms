//! `RedisUrl` — a Redis connection URL whose embedded password never
//! escapes through a secondary channel (ledger class F17).
//!
//! A `redis://user:password@host` URL is a credential. The raw value is
//! reachable only through [`RedisUrl::as_str`] (the connect path);
//! `Debug`, `Display` (log lines like `info!(url = %cfg.redis_url)`),
//! and `Serialize` (the Lua-facing config exposure) all mask the
//! userinfo password as `***`.

use serde::{Deserialize, Serialize, Serializer};
use std::fmt;

/// A Redis URL with password-masking `Debug`/`Display`/`Serialize`.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub struct RedisUrl(String);

impl RedisUrl {
    /// The real URL, for the connect path only.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the URL is unset.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The URL with any userinfo password replaced by `***`.
    #[must_use]
    pub fn masked(&self) -> String {
        mask_url_password(&self.0)
    }
}

impl From<String> for RedisUrl {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for RedisUrl {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl fmt::Debug for RedisUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RedisUrl({:?})", self.masked())
    }
}

impl fmt::Display for RedisUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.masked())
    }
}

impl Serialize for RedisUrl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.masked())
    }
}

/// A database connection string (Postgres URL or libpq conninfo) whose
/// password never escapes through Debug/Display/Serialize (ledger class
/// F17 — the same treatment `RedisUrl` gives the Redis password).
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub struct DbUrl(String);

impl DbUrl {
    /// The real connection string, for the connect path only.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The connection string with any password masked as `***` — both
    /// the URL form (`postgres://user:pw@host`) and the libpq
    /// key=value form (`password=pw`).
    #[must_use]
    pub fn masked(&self) -> String {
        mask_conninfo_password(&mask_url_password(&self.0))
    }
}

impl From<String> for DbUrl {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for DbUrl {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl fmt::Debug for DbUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DbUrl({:?})", self.masked())
    }
}

impl fmt::Display for DbUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.masked())
    }
}

impl Serialize for DbUrl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.masked())
    }
}

/// Mask `password=...` in a libpq key=value conninfo string.
fn mask_conninfo_password(s: &str) -> String {
    s.split_whitespace()
        .map(|kv| {
            if kv.starts_with("password=") {
                "password=***".to_string()
            } else {
                kv.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Replace the password half of a URL's userinfo with `***`.
///
/// `scheme://user:secret@host` → `scheme://user:***@host`. URLs without
/// userinfo (no `@` before the first path segment) pass through
/// unchanged.
fn mask_url_password(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let rest = &url[scheme_end + 3..];

    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];

    let Some(at) = authority.rfind('@') else {
        return url.to_string();
    };
    let userinfo = &authority[..at];

    let Some(colon) = userinfo.find(':') else {
        return url.to_string();
    };

    format!(
        "{}://{}:***{}",
        &url[..scheme_end],
        &userinfo[..colon],
        &rest[at..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_password_in_all_channels() {
        let url = RedisUrl::from("redis://user:hunter2@localhost:6379/1");

        assert_eq!(url.as_str(), "redis://user:hunter2@localhost:6379/1");
        assert_eq!(url.masked(), "redis://user:***@localhost:6379/1");
        assert!(!format!("{url:?}").contains("hunter2"));
        assert!(!format!("{url}").contains("hunter2"));
        let json = serde_json::to_string(&url).unwrap();
        assert!(!json.contains("hunter2"));
    }

    #[test]
    fn urls_without_credentials_pass_through() {
        for plain in ["redis://127.0.0.1:6379", "rediss://host:6380/2", ""] {
            let url = RedisUrl::from(plain);
            assert_eq!(url.masked(), plain);
        }
        // Password-only form (leading colon) is still masked.
        assert_eq!(
            RedisUrl::from("redis://:pw@host").masked(),
            "redis://:***@host"
        );
    }

    #[test]
    fn db_url_masks_both_conninfo_forms() {
        let url = DbUrl::from("postgres://crap:hunter2@db.internal/crap_cms");
        assert!(!format!("{url}").contains("hunter2"));
        assert!(!format!("{url:?}").contains("hunter2"));
        assert!(!serde_json::to_string(&url).unwrap().contains("hunter2"));
        assert_eq!(url.as_str(), "postgres://crap:hunter2@db.internal/crap_cms");

        let kv = DbUrl::from("host=localhost user=crap password=hunter2 dbname=crap_cms");
        assert_eq!(
            kv.masked(),
            "host=localhost user=crap password=*** dbname=crap_cms"
        );
    }

    #[test]
    fn deserializes_from_plain_string() {
        let url: RedisUrl = serde_json::from_str("\"redis://u:p@h\"").unwrap();
        assert_eq!(url.as_str(), "redis://u:p@h");
    }
}
