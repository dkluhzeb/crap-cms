//! Field validation error types returned by the hook system.

use std::collections::HashMap;
use std::fmt;

/// A single field validation error.
#[derive(Debug, Clone)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

/// Structured validation error containing per-field messages.
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub errors: Vec<FieldError>,
}

impl ValidationError {
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
