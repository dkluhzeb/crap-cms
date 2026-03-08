//! Field validation error types returned by the hook system.

use std::collections::HashMap;
use std::fmt;

/// A single field validation error.
#[derive(Debug, Clone)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

impl FieldError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self { field: field.into(), message: message.into() }
    }
}

/// Structured validation error containing per-field messages.
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub errors: Vec<FieldError>,
}

impl ValidationError {
    pub fn new(errors: Vec<FieldError>) -> Self {
        Self { errors }
    }


    /// Convert errors into a field-name-keyed map for template rendering.
    pub fn to_field_map(&self) -> HashMap<String, String> {
        self.errors.iter()
            .map(|e| (e.field.clone(), e.message.clone()))
            .collect()
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msgs: Vec<String> = self.errors.iter()
            .map(|e| format!("{}: {}", e.field, e.message))
            .collect();
        write!(f, "Validation failed: {}", msgs.join("; "))
    }
}

impl std::error::Error for ValidationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_field_map_single_error() {
        let ve = ValidationError {
            errors: vec![FieldError { field: "title".into(), message: "required".into() }],
        };
        let map = ve.to_field_map();
        assert_eq!(map.get("title").unwrap(), "required");
    }

    #[test]
    fn to_field_map_multiple_errors() {
        let ve = ValidationError {
            errors: vec![
                FieldError { field: "title".into(), message: "required".into() },
                FieldError { field: "email".into(), message: "invalid".into() },
            ],
        };
        let map = ve.to_field_map();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("title").unwrap(), "required");
        assert_eq!(map.get("email").unwrap(), "invalid");
    }

    #[test]
    fn to_field_map_duplicate_field_last_wins() {
        let ve = ValidationError {
            errors: vec![
                FieldError { field: "title".into(), message: "first error".into() },
                FieldError { field: "title".into(), message: "second error".into() },
            ],
        };
        let map = ve.to_field_map();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("title").unwrap(), "second error");
    }

    #[test]
    fn display_format() {
        let ve = ValidationError {
            errors: vec![
                FieldError { field: "title".into(), message: "required".into() },
                FieldError { field: "email".into(), message: "invalid".into() },
            ],
        };
        let s = ve.to_string();
        assert!(s.contains("title: required"));
        assert!(s.contains("email: invalid"));
        assert!(s.starts_with("Validation failed:"));
    }

    #[test]
    fn display_empty_errors() {
        let ve = ValidationError { errors: vec![] };
        let s = ve.to_string();
        assert_eq!(s, "Validation failed: ");
    }
}
