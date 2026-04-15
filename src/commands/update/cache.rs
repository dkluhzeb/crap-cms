//! Update-check cache stored under `$XDG_CACHE_HOME/crap-cms/update-check.json`.
//!
//! The cache file holds the latest release tag we saw from GitHub and when
//! we saw it. The serve startup nudge reads this file and skips the nudge if
//! the cache is stale or missing — never blocks startup on network I/O.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};
use tracing::debug;

/// How long a cache entry is considered "fresh" for the startup nudge.
pub const CACHE_TTL_HOURS: i64 = 24;

/// Hard cap on how many bytes we'll read from the cache file. The real file
/// is well under 1 KB; a larger file means corruption, accidental write, or
/// tampering — we don't want a `fs::read` to slurp gigabytes on startup.
const MAX_CACHE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCache {
    pub checked_at: DateTime<Utc>,
    pub latest: String,
}

/// Default cache path: `$XDG_CACHE_HOME/crap-cms/update-check.json`.
pub fn default_path() -> Option<PathBuf> {
    Some(cache_dir()?.join("update-check.json"))
}

fn cache_dir() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("XDG_CACHE_HOME")
        && !val.is_empty()
    {
        return Some(PathBuf::from(val).join("crap-cms"));
    }
    std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|home| PathBuf::from(home).join(".cache").join("crap-cms"))
}

/// Write the cache atomically (write to tmp, rename).
pub fn write_at(path: &Path, cache: &UpdateCache) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(cache).context("serializing update cache")?;
    fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

/// Read the cache (returns `None` on any error, including "not present",
/// "too large", or "parse error"). Never panics — the startup nudge path
/// must never block serve.
pub fn read_at(path: &Path) -> Option<UpdateCache> {
    let file = File::open(path).ok()?;

    // Size-cap the read so a corrupt or tampered cache file can't slurp
    // unbounded memory at startup.
    let mut bytes = Vec::with_capacity(1024);
    file.take(MAX_CACHE_BYTES).read_to_end(&mut bytes).ok()?;

    match serde_json::from_slice::<UpdateCache>(&bytes) {
        Ok(cache) => Some(cache),
        Err(e) => {
            debug!(
                "ignoring malformed update-check cache at {}: {e}",
                path.display()
            );
            None
        }
    }
}

/// Return the cached latest tag if the cache is present AND fresh (within
/// `CACHE_TTL_HOURS` of `now`). Used by the serve startup nudge.
pub fn fresh_latest_at(path: &Path, now: DateTime<Utc>) -> Option<String> {
    let cache = read_at(path)?;
    let age = now.signed_duration_since(cache.checked_at);
    if age < Duration::hours(CACHE_TTL_HOURS) && age >= Duration::zero() {
        Some(cache.latest)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trip_write_read() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("crap-cms").join("update-check.json");
        let entry = UpdateCache {
            checked_at: Utc::now(),
            latest: "v0.1.0-alpha.5".to_string(),
        };
        write_at(&path, &entry).unwrap();
        let back = read_at(&path).unwrap();
        assert_eq!(back.latest, entry.latest);
    }

    #[test]
    fn fresh_returns_latest_when_within_ttl() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("update-check.json");
        let now = Utc::now();
        let entry = UpdateCache {
            checked_at: now - Duration::hours(1),
            latest: "v0.1.0-alpha.5".to_string(),
        };
        write_at(&path, &entry).unwrap();

        let fresh = fresh_latest_at(&path, now).unwrap();
        assert_eq!(fresh, "v0.1.0-alpha.5");
    }

    #[test]
    fn fresh_returns_none_when_ttl_exceeded() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("update-check.json");
        let now = Utc::now();
        let entry = UpdateCache {
            checked_at: now - Duration::hours(CACHE_TTL_HOURS + 1),
            latest: "v0.1.0-alpha.5".to_string(),
        };
        write_at(&path, &entry).unwrap();
        assert!(fresh_latest_at(&path, now).is_none());
    }

    #[test]
    fn fresh_returns_none_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("missing.json");
        assert!(fresh_latest_at(&path, Utc::now()).is_none());
    }

    #[test]
    fn oversized_cache_file_is_ignored_safely() {
        // Write a 128KB payload (twice the cap). read_at must return None
        // without panicking or loading the whole file, so the startup nudge
        // stays safe against a corrupt/tampered cache.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("update-check.json");
        let junk = vec![b'x'; 128 * 1024];
        fs::write(&path, &junk).unwrap();

        assert!(read_at(&path).is_none());
    }

    #[test]
    fn malformed_json_is_ignored_safely() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("update-check.json");
        fs::write(&path, b"{not json").unwrap();

        assert!(read_at(&path).is_none());
    }

    #[test]
    fn fresh_ignores_entries_timestamped_in_the_future() {
        // Clock skew or hand-edited cache — don't treat it as fresh.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache.json");
        let now = Utc::now();
        let entry = UpdateCache {
            checked_at: now + Duration::hours(2),
            latest: "v999.0.0".to_string(),
        };
        write_at(&path, &entry).unwrap();
        assert!(fresh_latest_at(&path, now).is_none());
    }
}
