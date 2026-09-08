//! Resolution of the auth secret: the configured `[auth] secret`, or — when
//! that is empty — a random secret generated once and persisted under
//! `data/.jwt_secret`.
//!
//! Resolved at config load, so every consumer of `config.auth.secret` (JWT
//! signing, `crap.crypto`, TOTP sealing, signed upload URLs) keys off the
//! SAME value. Before this lived in `serve`'s startup, only the JWT provider
//! saw the generated secret; the other consumers read the raw (empty)
//! config value and derived their key from `""`.

use std::{fs, path::Path};

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

        Ok(())
    }
}

/// Load the persisted secret, or generate and persist a new one.
fn load_or_generate(config_dir: &Path) -> Result<String> {
    let secret_path = config_dir.join("data").join(".jwt_secret");

    if let Ok(s) = fs::read_to_string(&secret_path)
        && !s.trim().is_empty()
    {
        debug!("Using persisted auth secret from {}", secret_path.display());

        return Ok(s.trim().to_string());
    }

    let secret = nanoid!(64);
    let _ = fs::create_dir_all(secret_path.parent().expect("path has parent"));
    write_owner_only(&secret_path, &secret).with_context(|| {
        format!(
            "Failed to persist the auth secret to {} — cannot run with an ephemeral secret \
             (all sessions would be lost on restart)",
            secret_path.display()
        )
    })?;

    Ok(secret)
}

/// Create the secret file with owner-only permissions from the start — a
/// plain `fs::write` followed by `set_permissions` leaves a window in which the
/// file is readable under the process umask.
#[cfg(unix)]
fn write_owner_only(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents.as_bytes())
}

#[cfg(not(unix))]
fn write_owner_only(path: &Path, contents: &str) -> std::io::Result<()> {
    fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_generates_and_persists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut auth = AuthConfig::default(); // secret is empty

        auth.resolve_secret(tmp.path()).unwrap();
        assert!(!auth.secret.is_empty());

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
        assert_eq!(auth.secret.into_inner(), "my-explicit-secret");
        assert!(!tmp.path().join("data").join(".jwt_secret").exists());
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
        use std::os::unix::fs::PermissionsExt as _;

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
