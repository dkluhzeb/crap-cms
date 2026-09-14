//! Resolution of the auth secret: the configured `[auth] secret`, or — when
//! that is empty — a random secret generated once and persisted under
//! `data/.jwt_secret`.
//!
//! Resolved at config load, so every consumer of `config.auth.secret` (JWT
//! signing, `crap.crypto`, TOTP sealing, signed upload URLs) keys off the
//! SAME value. Before this lived in `serve`'s startup, only the JWT provider
//! saw the generated secret; the other consumers read the raw (empty)
//! config value and derived their key from `""`.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Write as _},
    path::Path,
};

use anyhow::{Context as _, Result};
use nanoid::nanoid;
use tracing::debug;

use crate::{config::AuthConfig, core::JwtSecret};

impl AuthConfig {
    /// Replace an empty `secret` with the persisted generated one (creating
    /// it on first use). A configured secret is left untouched.
    ///
    /// # Errors
    ///
    /// Returns an error when a fresh secret cannot be persisted: an ephemeral
    /// secret would invalidate every session, sealed TOTP secret, and
    /// ciphertext on the next restart, so the process must not run with one.
    pub fn resolve_secret(&mut self, config_dir: &Path) -> Result<()> {
        if !self.secret.is_empty() {
            return Ok(());
        }

        self.secret = JwtSecret::new(load_or_generate(config_dir)?);
        self.secret_generated = true;

        Ok(())
    }
}

/// Load the persisted secret, or generate and persist a new one. Generation
/// runs under an exclusive lock and re-reads the file once it holds it, so
/// processes starting together agree on one secret.
fn load_or_generate(config_dir: &Path) -> Result<String> {
    let data_dir = config_dir.join("data");
    let secret_path = data_dir.join(".jwt_secret");

    if let Some(secret) = read_secret(&secret_path)? {
        return Ok(secret);
    }

    fs::create_dir_all(&data_dir).with_context(|| cannot_persist(&secret_path))?;
    let _lock = lock_secret(&data_dir).with_context(|| cannot_persist(&secret_path))?;

    // A staged secret a crash left behind is unused, and holds a secret.
    remove_staged_secrets(&data_dir);

    // Another process may have written it while this one waited for the lock.
    if let Some(secret) = read_secret(&secret_path)? {
        return Ok(secret);
    }

    let secret = nanoid!(64);
    persist_secret(&secret_path, secret.as_bytes())
        .with_context(|| cannot_persist(&secret_path))?;

    Ok(secret)
}

/// Why a secret that can't be set up or persisted stops the process: running
/// with an ephemeral one would invalidate everything keyed by it on restart.
fn cannot_persist(path: &Path) -> String {
    format!(
        "Failed to persist the auth secret to {} — cannot run with an ephemeral secret \
         (all sessions would be lost on restart)",
        path.display()
    )
}

/// The persisted secret — `None` when there is none yet: no file, or an empty
/// one a crash left behind. A file that exists but can't be read is an error,
/// never replaced: every session and sealed value is keyed by the secret in it.
fn read_secret(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(None),
        Ok(s) => {
            debug!("Using persisted auth secret from {}", path.display());

            Ok(Some(s.trim().to_string()))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => {
            Err(e).with_context(|| format!("Failed to read the auth secret at {}", path.display()))
        }
    }
}

/// Remove the staged secret files in `data_dir` — left by a process that died
/// between writing and renaming one.
fn remove_staged_secrets(data_dir: &Path) {
    let Ok(entries) = fs::read_dir(data_dir) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".jwt_secret.new-")
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Hold an exclusive lock on `data/.jwt_secret.lock` until the returned file is
/// dropped.
fn lock_secret(data_dir: &Path) -> Result<File> {
    let path = data_dir.join(".jwt_secret.lock");

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    file.lock()
        .with_context(|| format!("Failed to lock {}", path.display()))?;

    Ok(file)
}

/// Write a generated secret to `path` through a staged file renamed into place,
/// so no reader ever sees a half-written secret. The staged file is removed
/// when any step fails.
fn persist_secret(path: &Path, secret: &[u8]) -> io::Result<()> {
    let staged = path.with_file_name(format!(".jwt_secret.new-{}", nanoid!(8)));

    let result = write_new_owner_only(&staged, secret).and_then(|()| fs::rename(&staged, path));
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }

    result
}

/// Create a secret file with owner-only permissions from the start — a plain
/// `fs::write` followed by `set_permissions` leaves a window in which the file
/// is readable under the process umask. Never overwrites an existing file.
///
/// # Errors
///
/// Returns an error if the file already exists or can't be created or written.
#[cfg(unix)]
pub(crate) fn write_new_owner_only(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;

    file.write_all(contents)?;
    file.sync_all()
}

/// See the unix variant; no permission bits to set here.
///
/// # Errors
///
/// Returns an error if the file already exists or can't be created or written.
#[cfg(not(unix))]
pub(crate) fn write_new_owner_only(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;

    file.write_all(contents)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    use std::thread;

    use super::*;

    #[test]
    fn resolve_generates_and_persists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut auth = AuthConfig::default(); // secret is empty

        auth.resolve_secret(tmp.path()).unwrap();
        assert!(!auth.secret.is_empty());
        assert!(auth.secret_generated);

        let secret_path = tmp.path().join("data").join(".jwt_secret");
        let persisted = fs::read_to_string(&secret_path).unwrap();
        assert_eq!(persisted, auth.secret.clone().into_inner());
    }

    #[test]
    fn resolve_reuses_persisted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut first = AuthConfig::default();
        let mut second = AuthConfig::default();

        first.resolve_secret(tmp.path()).unwrap();
        second.resolve_secret(tmp.path()).unwrap();

        assert_eq!(
            first.secret.into_inner(),
            second.secret.into_inner(),
            "must reuse the persisted secret"
        );
    }

    #[test]
    fn resolve_keeps_a_configured_value() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut auth = AuthConfig {
            secret: JwtSecret::new("my-explicit-secret"),
            ..Default::default()
        };

        auth.resolve_secret(tmp.path()).unwrap();
        assert!(!auth.secret_generated);
        assert_eq!(auth.secret.into_inner(), "my-explicit-secret");
        assert!(!tmp.path().join("data").join(".jwt_secret").exists());
    }

    /// Regression: an empty `.jwt_secret` — left by a crash between creating
    /// and writing it — stopped startup, because the secret file is only ever
    /// created, never written over.
    #[test]
    fn resolve_replaces_an_empty_secret_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join(".jwt_secret"), "").unwrap();

        let mut auth = AuthConfig::default();
        auth.resolve_secret(tmp.path()).unwrap();

        let secret_path = data.join(".jwt_secret");
        assert_eq!(
            fs::read_to_string(&secret_path).unwrap(),
            auth.secret.clone().into_inner()
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&secret_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let staged = fs::read_dir(&data)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".jwt_secret.new-")
            })
            .count();
        assert_eq!(staged, 0, "no staged file left");
    }

    /// Regression: a secret file that exists but can't be read was treated as
    /// missing and replaced, losing the secret everything is keyed by.
    #[test]
    fn an_unreadable_secret_file_is_an_error_not_replaced() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // A directory where the file belongs: it exists, and reading it fails.
        let secret_path = tmp.path().join("data").join(".jwt_secret");
        fs::create_dir_all(&secret_path).unwrap();

        let mut auth = AuthConfig::default();

        assert!(auth.resolve_secret(tmp.path()).is_err());
        assert!(
            secret_path.is_dir(),
            "the unreadable secret is left in place"
        );
    }

    /// Regression: two processes resolving the secret at once could each
    /// generate one, and both run with a secret the other didn't persist.
    #[test]
    fn concurrent_resolves_agree_on_one_secret() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let dir = dir.clone();
                thread::spawn(move || {
                    let mut auth = AuthConfig::default();
                    auth.resolve_secret(&dir).unwrap();
                    auth.secret.into_inner()
                })
            })
            .collect();
        let secrets: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let persisted = fs::read_to_string(dir.join("data").join(".jwt_secret")).unwrap();
        assert!(secrets.iter().all(|s| *s == persisted), "{secrets:?}");
    }

    /// Regression: a staged secret left by a process that died before renaming
    /// it stayed in `data/` for good.
    #[test]
    fn staged_secrets_left_by_a_crash_are_removed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join(".jwt_secret.new-crashed"), "leftover").unwrap();

        AuthConfig::default().resolve_secret(tmp.path()).unwrap();

        assert!(!data.join(".jwt_secret.new-crashed").exists());
    }

    /// A write failure must not silently fall back to an ephemeral secret
    /// (which would lose every session on restart).
    #[test]
    fn resolve_fails_on_unwritable_path() {
        let err = AuthConfig::default()
            .resolve_secret(Path::new("/nonexistent/path"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("ephemeral secret"), "unexpected error: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn generated_file_is_owner_only_from_creation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        AuthConfig::default().resolve_secret(tmp.path()).unwrap();

        let mode = fs::metadata(tmp.path().join("data").join(".jwt_secret"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
