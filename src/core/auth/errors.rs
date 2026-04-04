//! Error types for auth operations.

use std::fmt;

/// Error types for password reset token operations.
#[derive(Debug, Clone, PartialEq)]
pub enum ResetTokenError {
    /// The token was not found in any auth collection.
    NotFound,
    /// The token was found but has expired.
    Expired,
}

impl fmt::Display for ResetTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "Invalid reset token"),
            Self::Expired => write!(f, "Reset token has expired"),
        }
    }
}

impl std::error::Error for ResetTokenError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!(ResetTokenError::NotFound.to_string(), "Invalid reset token");
        assert_eq!(
            ResetTokenError::Expired.to_string(),
            "Reset token has expired"
        );
    }

    #[test]
    fn downcast_roundtrip() {
        let err: anyhow::Error = ResetTokenError::Expired.into();
        assert_eq!(
            err.downcast_ref::<ResetTokenError>(),
            Some(&ResetTokenError::Expired)
        );

        let err: anyhow::Error = ResetTokenError::NotFound.into();
        assert_eq!(
            err.downcast_ref::<ResetTokenError>(),
            Some(&ResetTokenError::NotFound)
        );
    }
}
