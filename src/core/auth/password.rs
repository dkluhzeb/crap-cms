//! Password provider trait and Argon2id implementation.

use std::sync::{Arc, LazyLock};

use anyhow::{Result, anyhow};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};

use super::HashedPassword;

/// Thread-safe shared reference to a password provider.
pub type SharedPasswordProvider = Arc<dyn PasswordProvider>;

/// Object-safe password provider trait.
///
/// Abstracts password hashing and verification. The default implementation
/// uses Argon2id. Rarely swapped — exists for testability and potential
/// future backends (bcrypt, scrypt, etc.).
pub trait PasswordProvider: Send + Sync {
    /// Hash a password for storage.
    fn hash_password(&self, password: &str) -> Result<HashedPassword>;

    /// Verify a password against a stored hash.
    fn verify_password(&self, password: &str, hash: &str) -> Result<bool>;

    /// Timing-safe dummy verification (prevents user enumeration).
    fn dummy_verify(&self);

    /// Backend identifier.
    fn kind(&self) -> &'static str;
}

/// Pre-computed Argon2 hash for dummy verification timing equalization.
static DUMMY_HASH: LazyLock<HashedPassword> = LazyLock::new(|| {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(b"__crap_dummy_timing__", &salt)
        .expect("dummy hash");
    HashedPassword::new(hash.to_string())
});

/// Argon2id password provider.
pub struct Argon2PasswordProvider;

impl PasswordProvider for Argon2PasswordProvider {
    fn hash_password(&self, password: &str) -> Result<HashedPassword> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| anyhow!("Password hashing failed: {}", e))?;
        Ok(HashedPassword::new(hash.to_string()))
    }

    fn verify_password(&self, password: &str, hash: &str) -> Result<bool> {
        let parsed =
            PasswordHash::new(hash).map_err(|e| anyhow!("Invalid password hash: {}", e))?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok())
    }

    fn dummy_verify(&self) {
        let parsed = PasswordHash::new(DUMMY_HASH.as_ref()).expect("dummy hash is valid");
        let _ = Argon2::default().verify_password(b"x", &parsed);
    }

    fn kind(&self) -> &'static str {
        "argon2"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify() {
        let p = Argon2PasswordProvider;
        let hash = p.hash_password("secret123").unwrap();
        assert!(p.verify_password("secret123", hash.as_ref()).unwrap());
        assert!(!p.verify_password("wrong", hash.as_ref()).unwrap());
    }

    #[test]
    fn dummy_verify_no_panic() {
        Argon2PasswordProvider.dummy_verify();
    }

    #[test]
    fn kind_is_argon2() {
        assert_eq!(Argon2PasswordProvider.kind(), "argon2");
    }

    #[test]
    fn empty_password_roundtrips() {
        let p = Argon2PasswordProvider;
        let hash = p.hash_password("").unwrap();
        assert!(p.verify_password("", hash.as_ref()).unwrap());
        assert!(!p.verify_password("notempty", hash.as_ref()).unwrap());
    }
}
