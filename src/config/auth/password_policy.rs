//! Password strength requirements applied to every password-set path.

use std::{
    error::Error,
    fmt::{Display, Formatter, Result as FmtResult},
};

use serde::{Deserialize, Serialize};

/// Password strength requirements. Applied to all password-setting paths:
/// user creation (admin, gRPC, CLI), password reset, and password update.
#[derive(Debug, Clone, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
#[serde(default, deny_unknown_fields)]
pub struct PasswordPolicy {
    /// Minimum password length. Default: 8. Recommended: 12+ for modern security.
    pub min_length: usize,
    /// Maximum password length. Default: 128. Prevents `DoS` via Argon2 on huge inputs.
    pub max_length: usize,
    /// Require at least one uppercase letter (A-Z). Default: false.
    pub require_uppercase: bool,
    /// Require at least one lowercase letter (a-z). Default: false.
    pub require_lowercase: bool,
    /// Require at least one digit (0-9). Default: false.
    pub require_digit: bool,
    /// Require at least one special character (non-alphanumeric). Default: false.
    pub require_special: bool,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            min_length: 8,
            max_length: 128,
            require_uppercase: false,
            require_lowercase: false,
            require_digit: false,
            require_special: false,
        }
    }
}

/// The first requirement a password fails. Its `Display` is the English
/// message every surface reports; [`translation_key`](Self::translation_key)
/// and [`params`](Self::params) let the admin UI render it in the viewer's
/// locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordViolation {
    /// Fewer than `min` characters.
    TooShort {
        min: usize,
    },
    /// More than `max` bytes.
    TooLong {
        max: usize,
    },
    MissingUppercase,
    MissingLowercase,
    MissingDigit,
    MissingSpecial,
}

impl PasswordViolation {
    /// The admin translation key for this violation.
    #[must_use]
    pub fn translation_key(&self) -> &'static str {
        match self {
            Self::TooShort { .. } => "validation.password_min_length",
            Self::TooLong { .. } => "validation.password_max_bytes",
            Self::MissingUppercase => "validation.password_uppercase",
            Self::MissingLowercase => "validation.password_lowercase",
            Self::MissingDigit => "validation.password_digit",
            Self::MissingSpecial => "validation.password_special",
        }
    }

    /// The interpolation params [`translation_key`](Self::translation_key)'s
    /// message uses.
    #[must_use]
    pub fn params(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::TooShort { min } => vec![("min", min.to_string())],
            Self::TooLong { max } => vec![("max", max.to_string())],
            _ => Vec::new(),
        }
    }
}

impl Display for PasswordViolation {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::TooShort { min } => write!(f, "Password must be at least {min} characters"),
            Self::TooLong { max } => write!(f, "Password must be at most {max} bytes"),
            Self::MissingUppercase => {
                f.write_str("Password must contain at least one uppercase letter")
            }
            Self::MissingLowercase => {
                f.write_str("Password must contain at least one lowercase letter")
            }
            Self::MissingDigit => f.write_str("Password must contain at least one digit"),
            Self::MissingSpecial => {
                f.write_str("Password must contain at least one special character")
            }
        }
    }
}

impl Error for PasswordViolation {}

impl PasswordPolicy {
    /// Validate a password against this policy. Returns `Ok(())` if the password
    /// meets all requirements, or the first requirement it fails.
    ///
    /// # Errors
    ///
    /// Returns the [`PasswordViolation`] for the first failed length,
    /// character-class, or other configured requirement.
    pub fn validate(&self, password: &str) -> Result<(), PasswordViolation> {
        if password.chars().count() < self.min_length {
            return Err(PasswordViolation::TooShort {
                min: self.min_length,
            });
        }

        // Max length uses byte length intentionally: Argon2 hashes the raw bytes,
        // so limiting bytes prevents DoS via large multi-byte payloads.
        if password.len() > self.max_length {
            return Err(PasswordViolation::TooLong {
                max: self.max_length,
            });
        }

        let lacks =
            |required: bool, class: fn(char) -> bool| required && !password.chars().any(class);

        if lacks(self.require_uppercase, |c: char| c.is_ascii_uppercase()) {
            return Err(PasswordViolation::MissingUppercase);
        }

        if lacks(self.require_lowercase, |c: char| c.is_ascii_lowercase()) {
            return Err(PasswordViolation::MissingLowercase);
        }

        if lacks(self.require_digit, |c: char| c.is_ascii_digit()) {
            return Err(PasswordViolation::MissingDigit);
        }

        if lacks(self.require_special, |c: char| !c.is_alphanumeric()) {
            return Err(PasswordViolation::MissingSpecial);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;

    #[test]
    fn password_policy_defaults() {
        let policy = PasswordPolicy::default();
        assert_eq!(policy.min_length, 8);
        assert_eq!(policy.max_length, 128);
        assert!(!policy.require_uppercase);
        assert!(!policy.require_lowercase);
        assert!(!policy.require_digit);
        assert!(!policy.require_special);
    }

    #[test]
    fn password_policy_accepts_valid() {
        let policy = PasswordPolicy::default();
        assert!(policy.validate("abcdefgh").is_ok());
        assert!(policy.validate("12345678").is_ok());
    }

    #[test]
    fn password_policy_rejects_too_short() {
        let policy = PasswordPolicy {
            min_length: 8,
            ..Default::default()
        };
        assert!(policy.validate("short").is_err());
        assert!(policy.validate("1234567").is_err());
        assert!(policy.validate("12345678").is_ok());
    }

    #[test]
    fn password_policy_rejects_too_long() {
        let policy = PasswordPolicy {
            max_length: 10,
            ..Default::default()
        };
        assert!(policy.validate("12345678").is_ok());
        assert!(policy.validate("12345678901").is_err());
    }

    /// Regression: `max_length` error message said "characters" but the check uses byte length.
    #[test]
    fn password_policy_max_length_error_says_bytes() {
        let policy = PasswordPolicy {
            max_length: 10,
            ..Default::default()
        };
        let err = policy.validate("12345678901").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bytes"),
            "error message should say 'bytes', got: {msg}"
        );
        assert!(
            !msg.contains("characters"),
            "error message should not say 'characters', got: {msg}"
        );
    }

    #[test]
    fn password_policy_require_uppercase() {
        let policy = PasswordPolicy {
            require_uppercase: true,
            ..Default::default()
        };
        assert!(policy.validate("alllower").is_err());
        assert!(policy.validate("hasUpper1").is_ok());
    }

    #[test]
    fn password_policy_require_lowercase() {
        let policy = PasswordPolicy {
            require_lowercase: true,
            ..Default::default()
        };
        assert!(policy.validate("ALLUPPER").is_err());
        assert!(policy.validate("HASLOWERa").is_ok());
    }

    #[test]
    fn password_policy_require_digit() {
        let policy = PasswordPolicy {
            require_digit: true,
            ..Default::default()
        };
        assert!(policy.validate("nodigits").is_err());
        assert!(policy.validate("hasdigit1").is_ok());
    }

    #[test]
    fn password_policy_require_special() {
        let policy = PasswordPolicy {
            require_special: true,
            ..Default::default()
        };
        assert!(policy.validate("nospecial1").is_err());
        assert!(policy.validate("special!1").is_ok());
    }

    #[test]
    fn password_policy_all_requirements() {
        let policy = PasswordPolicy {
            min_length: 8,
            max_length: 128,
            require_uppercase: true,
            require_lowercase: true,
            require_digit: true,
            require_special: true,
        };
        assert!(policy.validate("Abc1234!").is_ok());
        assert!(policy.validate("abc1234!").is_err(), "missing uppercase");
        assert!(policy.validate("ABC1234!").is_err(), "missing lowercase");
        assert!(policy.validate("Abcdefg!").is_err(), "missing digit");
        assert!(policy.validate("Abc12345").is_err(), "missing special");
        assert!(policy.validate("Ac1!").is_err(), "too short");
    }

    /// Each violation names its own translation key and the params that key
    /// interpolates, so the admin renders it in the viewer's locale.
    #[test]
    fn violations_carry_their_translation_key_and_params() {
        let policy = PasswordPolicy {
            min_length: 10,
            ..Default::default()
        };
        let err = policy.validate("short").unwrap_err();

        assert_eq!(err, PasswordViolation::TooShort { min: 10 });
        assert_eq!(err.translation_key(), "validation.password_min_length");
        assert_eq!(err.params(), vec![("min", "10".to_string())]);
        assert_eq!(err.to_string(), "Password must be at least 10 characters");
    }

    #[test]
    fn password_policy_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            r"
[auth.password_policy]
min_length = 12
require_uppercase = true
require_digit = true
",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.auth.password_policy.min_length, 12);
        assert!(config.auth.password_policy.require_uppercase);
        assert!(config.auth.password_policy.require_digit);
        assert!(!config.auth.password_policy.require_lowercase);
        assert!(!config.auth.password_policy.require_special);
    }
}
