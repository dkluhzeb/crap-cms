//! `RedisUrl` — a Redis connection URL whose embedded password never
//! escapes through a secondary channel.
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
/// password never escapes through Debug/Display/Serialize.
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
///
/// Follows what tokio-postgres accepts: whitespace around `=`, and
/// single-quoted values that may contain spaces (`password = 'hunter two'`).
fn mask_conninfo_password(s: &str) -> String {
    let (before, pw, after) = split_conninfo_password(s);
    match pw {
        Some(_) => format!("{before}password=***{after}"),
        None => s.to_string(),
    }
}

/// Split a conninfo string at its `password` entry: `(text before, the
/// password value, text after)`. The value is `None` when there is no
/// `password` key.
fn split_conninfo_password(s: &str) -> (&str, Option<String>, &str) {
    let mut cursor = 0;
    while cursor < s.len() {
        let rest = &s[cursor..];
        let trimmed = rest.trim_start();
        let start = cursor + (rest.len() - trimmed.len());
        if trimmed.is_empty() {
            break;
        }

        let key_len = trimmed.find(['=', ' ', '\t']).unwrap_or(trimmed.len());
        let key = &trimmed[..key_len];
        let after_key = trimmed[key_len..].trim_start();
        let Some(after_eq) = after_key.strip_prefix('=') else {
            cursor = start + key_len.max(1);
            continue;
        };
        let value_src = after_eq.trim_start();
        let value_start = s.len() - value_src.len();
        let (value, value_len) = if let Some(quoted) = value_src.strip_prefix('\'') {
            let end = quoted.find('\'').unwrap_or(quoted.len());
            (quoted[..end].to_string(), end + 2)
        } else {
            let end = value_src.find([' ', '\t']).unwrap_or(value_src.len());
            (value_src[..end].to_string(), end)
        };
        let value_end = (value_start + value_len).min(s.len());

        if key == "password" {
            return (&s[..start], Some(value), &s[value_end..]);
        }
        cursor = value_end.max(start + 1);
    }

    (s, None, "")
}

/// Replace the password half of a URL's userinfo with `***`.
///
/// `scheme://user:secret@host` → `scheme://user:***@host`. URLs without
/// userinfo pass through unchanged.
fn mask_url_password(url: &str) -> String {
    match split_url_password(url) {
        (_, None) => url.to_string(),
        (stripped, Some(_)) => {
            // Re-insert the mask where the password was: `user@` → `user:***@`.
            let Some(scheme_end) = stripped.find("://") else {
                return url.to_string();
            };
            let at = scheme_end + 3 + userinfo_at(&stripped[scheme_end + 3..]).unwrap_or(0);
            format!("{}:***{}", &stripped[..at], &stripped[at..])
        }
    }
}

/// Split a URL into `(url without the password, the password)`. The
/// authority is taken up to the LAST `@` before any `?`/`#`, so an
/// unencoded `/` inside the password does not truncate it.
fn split_url_password(url: &str) -> (String, Option<String>) {
    let Some(scheme_end) = url.find("://") else {
        return (url.to_string(), None);
    };
    let rest = &url[scheme_end + 3..];
    let Some(at) = userinfo_at(rest) else {
        return (url.to_string(), None);
    };
    let userinfo = &rest[..at];
    let Some(colon) = userinfo.find(':') else {
        return (url.to_string(), None);
    };

    let stripped = format!(
        "{}://{}{}",
        &url[..scheme_end],
        &userinfo[..colon],
        &rest[at..]
    );

    (stripped, Some(userinfo[colon + 1..].to_string()))
}

/// Index of the `@` that ends the userinfo in `rest` (the part after
/// `scheme://`), if any — the last `@` before the query/fragment.
fn userinfo_at(rest: &str) -> Option<usize> {
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    rest[..end].rfind('@')
}

impl DbUrl {
    /// The URL with its password removed, plus the password — for handing a
    /// connection string to a child process (`psql`) without the secret in
    /// its argument list. Works for both URL and libpq key=value forms.
    #[must_use]
    pub fn without_password(&self) -> (String, Option<String>) {
        let raw = self.as_str();
        if raw.contains("://") {
            return split_url_password(raw);
        }

        let (before, pw, after) = split_conninfo_password(raw);
        match pw {
            Some(pw) => (
                format!("{} {}", before.trim_end(), after.trim_start())
                    .trim()
                    .to_string(),
                Some(pw),
            ),
            None => (raw.to_string(), None),
        }
    }
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

    /// libpq forms tokio-postgres accepts — spaces around `=`, quoted values
    /// with spaces — are masked too, and an unencoded `/` in a URL password
    /// does not truncate the mask.
    #[test]
    fn masks_libpq_spacing_quotes_and_slash_passwords() {
        assert_eq!(
            mask_conninfo_password("host=h password = hunter2 dbname=d"),
            "host=h password=*** dbname=d"
        );
        assert_eq!(
            mask_conninfo_password("host=h password='hunter two' dbname=d"),
            "host=h password=*** dbname=d"
        );
        assert_eq!(
            mask_url_password("postgres://u:pa/ss@h/db?sslmode=require"),
            "postgres://u:***@h/db?sslmode=require"
        );
        assert_eq!(mask_url_password("postgres://h/db"), "postgres://h/db");
    }

    #[test]
    fn without_password_splits_both_forms() {
        let (url, pw) = DbUrl::from("postgres://u:hunter2@h/db").without_password();
        assert_eq!(url, "postgres://u@h/db");
        assert_eq!(pw.as_deref(), Some("hunter2"));

        let (conn, pw) = DbUrl::from("host=h password='hunter two' dbname=d").without_password();
        assert_eq!(conn, "host=h dbname=d");
        assert_eq!(pw.as_deref(), Some("hunter two"));

        let (same, none) = DbUrl::from("postgres://h/db").without_password();
        assert_eq!(same, "postgres://h/db");
        assert!(none.is_none());
    }
}
