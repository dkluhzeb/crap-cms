//! Field validation error types returned by the hook system.

use std::{collections::HashMap, fmt};

/// A single field validation error.
#[derive(Debug, Clone)]
pub struct FieldError {
    pub field: String,
    pub message: String,
    /// Translation key (e.g. "validation.required"). None for custom Lua validator messages.
    pub key: Option<String>,
    /// Interpolation params for the translation key (e.g. {"field": "title", "min": "5"}).
    pub params: HashMap<String, String>,
}

impl FieldError {
    /// Create an error without a translation key (used by custom Lua validators).
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            key: None,
            params: HashMap::new(),
        }
    }

    /// Create an error with a translation key and interpolation params.
    pub fn with_key(
        field: impl Into<String>,
        message: impl Into<String>,
        key: impl Into<String>,
        params: HashMap<String, String>,
    ) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            key: Some(key.into()),
            params,
        }
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
    /// When multiple errors exist for the same field, messages are joined with "; ".
    pub fn to_field_map(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for e in &self.errors {
            map.entry(e.field.clone())
                .and_modify(|existing: &mut String| {
                    existing.push_str("; ");
                    existing.push_str(&e.message);
                })
                .or_insert_with(|| e.message.clone());
        }
        map
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msgs: Vec<String> = self
            .errors
            .iter()
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
        let ve = ValidationError::new(vec![FieldError::new("title", "required")]);
        let map = ve.to_field_map();
        assert_eq!(map.get("title").unwrap(), "required");
    }

    #[test]
    fn to_field_map_multiple_errors() {
        let ve = ValidationError::new(vec![
            FieldError::new("title", "required"),
            FieldError::new("email", "invalid"),
        ]);
        let map = ve.to_field_map();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("title").unwrap(), "required");
        assert_eq!(map.get("email").unwrap(), "invalid");
    }

    #[test]
    fn to_field_map_duplicate_field_joins_with_separator() {
        let ve = ValidationError::new(vec![
            FieldError::new("title", "first error"),
            FieldError::new("title", "second error"),
        ]);
        let map = ve.to_field_map();
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get("title").unwrap(),
            "first error; second error",
            "Duplicate field errors should be joined with '; '"
        );
    }

    #[test]
    fn to_field_map_three_errors_same_field_all_joined() {
        let ve = ValidationError::new(vec![
            FieldError::new("email", "required"),
            FieldError::new("email", "invalid format"),
            FieldError::new("email", "already taken"),
        ]);
        let map = ve.to_field_map();
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get("email").unwrap(),
            "required; invalid format; already taken",
        );
    }

    #[test]
    fn to_field_map_mixed_unique_and_duplicate_fields() {
        let ve = ValidationError::new(vec![
            FieldError::new("title", "too short"),
            FieldError::new("email", "required"),
            FieldError::new("title", "contains profanity"),
        ]);
        let map = ve.to_field_map();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("title").unwrap(), "too short; contains profanity",);
        assert_eq!(map.get("email").unwrap(), "required");
    }

    #[test]
    fn with_key_stores_key_and_params() {
        let mut params = HashMap::new();
        params.insert("field".to_string(), "title".to_string());
        let err = FieldError::with_key(
            "title",
            "title is required",
            "validation.required",
            params.clone(),
        );
        assert_eq!(err.key.as_deref(), Some("validation.required"));
        assert_eq!(err.params, params);
        assert_eq!(err.message, "title is required");
    }

    #[test]
    fn new_has_no_key() {
        let err = FieldError::new("title", "custom error");
        assert!(err.key.is_none());
        assert!(err.params.is_empty());
    }

    #[test]
    fn display_format() {
        let ve = ValidationError::new(vec![
            FieldError::new("title", "required"),
            FieldError::new("email", "invalid"),
        ]);
        let s = ve.to_string();
        assert!(s.contains("title: required"));
        assert!(s.contains("email: invalid"));
        assert!(s.starts_with("Validation failed:"));
    }
}
