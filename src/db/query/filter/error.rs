//! The typed error of a query the caller got wrong — an unknown filter field
//! or path, an operand that does not fit its field, a sort the collection
//! cannot order by.

use crate::core::{FieldError, ValidationError};

/// A [`ValidationError`] naming `key` — the filter path, or `order_by` — as
/// `anyhow::Error`.
///
/// Every query-shape rejection goes through here, so each surface recognises
/// it by type (`ServiceError::Validation`) and answers invalid-argument, never
/// an internal fault a client would retry.
pub(crate) fn invalid_query(key: &str, message: impl Into<String>) -> anyhow::Error {
    ValidationError::new(vec![FieldError::new(key, message)]).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The error downcasts to a `ValidationError` naming the key.
    #[test]
    fn invalid_query_is_a_validation_error_naming_the_key() {
        let err = invalid_query("items.nope", "Unknown field 'nope'");

        let ve = err.downcast_ref::<ValidationError>().expect("typed");

        assert_eq!(ve.errors[0].field, "items.nope");
        assert_eq!(ve.errors[0].message, "Unknown field 'nope'");
    }
}
