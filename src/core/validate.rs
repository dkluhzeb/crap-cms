//! Field validation error types returned by the hook system.

use std::{collections::HashMap, fmt, sync::OnceLock};

use nanoid::nanoid;

use serde_json::{Map, Value, from_str, to_string};

/// Fixed part of the marker that tags an error message as a structured
/// validation failure raised from Lua. Never used on its own — see
/// [`hook_validation_prefix`].
const HOOK_VALIDATION_TAG: &str = "crap:validation-error:";

/// Per-process random suffix, so the marker cannot be guessed.
static HOOK_VALIDATION_NONCE: OnceLock<String> = OnceLock::new();

/// The marker that tags an error message as a structured validation failure.
///
/// A Lua hook can only fail by raising a string, so `crap.validation_error`
/// encodes its field errors as JSON after this marker and
/// [`ValidationError::from_hook_message`] is the one place that reads them
/// back. Both halves go through this function so the two ends cannot drift.
///
/// The marker carries a random per-process suffix because the channel would
/// otherwise be forgeable: a hook that interpolates user data into its
/// message — `error("rejected: " .. doc.slug)` — would let that data
/// impersonate a validation failure on a field of the attacker's choosing.
/// A value minted after the process started cannot be embedded in content
/// written before it, and is never disclosed.
#[must_use]
pub fn hook_validation_prefix() -> &'static str {
    HOOK_VALIDATION_NONCE.get_or_init(|| format!("{HOOK_VALIDATION_TAG}{}:", nanoid!(16)))
}

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

    /// Create an error with a translation key. Interpolation params for
    /// the i18n template are added with [`with_param`](Self::with_param).
    pub fn with_key(
        field: impl Into<String>,
        message: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            key: Some(key.into()),
            params: HashMap::new(),
        }
    }

    /// Add or override an interpolation parameter for the translation key.
    /// Chainable on construction:
    ///
    /// ```ignore
    /// FieldError::with_key(field_name, msg, "validation.length_min")
    ///     .with_param("field", display_name)
    ///     .with_param("min", min_len.to_string())
    /// ```
    #[must_use]
    pub fn with_param(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.insert(name.into(), value.into());
        self
    }
}

/// Structured validation error containing per-field messages.
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub errors: Vec<FieldError>,
}

impl ValidationError {
    #[must_use]
    pub fn new(errors: Vec<FieldError>) -> Self {
        Self { errors }
    }

    /// Convert errors into a field-name-keyed map for template rendering.
    /// When multiple errors exist for the same field, messages are joined with "; ".
    #[must_use]
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

    /// Encode as the single-line message a Lua hook raises. Single line
    /// because mlua appends a stack traceback, and the decoder reads only up
    /// to the first newline.
    #[must_use]
    pub fn to_hook_message(&self) -> String {
        let mut fields = Map::new();
        for e in &self.errors {
            fields.insert(e.field.clone(), Value::String(e.message.clone()));
        }

        let encoded = to_string(&Value::Object(fields)).unwrap_or_else(|_| "{}".to_string());

        format!("{}{encoded}", hook_validation_prefix())
    }

    /// Recover the field errors a hook encoded into its message.
    ///
    /// `None` when the message is an ordinary error — the caller then treats
    /// it as a plain hook failure.
    ///
    /// The marker may sit anywhere in the string — mlua wraps the raised
    /// value in `runtime error: …` and appends a traceback — but the JSON
    /// must run to the end of that line.
    #[must_use]
    pub fn from_hook_message(message: &str) -> Option<Self> {
        let prefix = hook_validation_prefix();
        let start = message.find(prefix)? + prefix.len();
        let rest = &message[start..];
        let encoded = rest.split('\n').next().unwrap_or(rest).trim_end();

        let Value::Object(fields) = from_str::<Value>(encoded).ok()? else {
            return None;
        };

        // No translation key: the hook author wrote the message, so there is
        // nothing to look up — and a key with no entry renders as the key.
        let errors: Vec<FieldError> = fields
            .into_iter()
            .map(|(field, message)| {
                let text = match message {
                    Value::String(s) => s,
                    other => other.to_string(),
                };

                FieldError::new(field, text)
            })
            .collect();

        (!errors.is_empty()).then(|| Self::new(errors))
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
        let err = FieldError::with_key("title", "title is required", "validation.required")
            .with_param("field", "title");
        assert_eq!(err.key.as_deref(), Some("validation.required"));
        assert_eq!(err.params.get("field").map(String::as_str), Some("title"));
        assert_eq!(err.message, "title is required");
    }

    #[test]
    fn with_param_chain_accumulates() {
        let err = FieldError::with_key("title", "min 5", "validation.length_min")
            .with_param("field", "Title")
            .with_param("min", "5");
        assert_eq!(err.params.get("field").map(String::as_str), Some("Title"));
        assert_eq!(err.params.get("min").map(String::as_str), Some("5"));
    }

    #[test]
    fn with_param_overrides_existing() {
        let err = FieldError::with_key("title", "msg", "validation.required")
            .with_param("field", "title")
            .with_param("field", "Title");
        assert_eq!(err.params.get("field").map(String::as_str), Some("Title"));
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
