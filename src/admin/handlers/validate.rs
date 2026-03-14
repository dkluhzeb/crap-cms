//! Shared types and helpers for validation endpoints.

use std::collections::HashMap;

use axum::{
    Json,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    admin::{Translations, handlers::shared::translate_validation_errors},
    core::validate::ValidationError,
};

/// JSON request body for validation endpoints.
#[derive(Deserialize)]
pub struct ValidateRequest {
    pub data: HashMap<String, Value>,
    #[serde(default)]
    pub draft: bool,
    pub locale: Option<String>,
}

/// Build a JSON response from a `ValidationError`, translating field errors via i18n.
pub fn validation_error_response(
    ve: &ValidationError,
    translations: &Translations,
    locale: &str,
) -> Response {
    let error_map = translate_validation_errors(ve, translations, locale);

    Json(json!({
        "valid": false,
        "errors": error_map,
    }))
    .into_response()
}

/// Build a JSON "valid: true" response.
pub fn validation_ok_response() -> Response {
    Json(json!({ "valid": true })).into_response()
}

/// Convert a `HashMap<String, Value>` into a `HashMap<String, String>` suitable for
/// the form processing pipeline (transform_select_has_many, extract_join_data_from_form).
///
/// Strings pass through, numbers/bools become their string representation, nulls become
/// empty strings, and arrays/objects become their JSON serialization.
pub fn values_to_string_map(data: &HashMap<String, Value>) -> HashMap<String, String> {
    data.iter()
        .map(|(k, v)| {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            (k.clone(), s)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_to_string_map_converts_types() {
        let mut data = HashMap::new();
        data.insert("title".to_string(), json!("Hello"));
        data.insert("count".to_string(), json!(42));
        data.insert("active".to_string(), json!(true));
        data.insert("empty".to_string(), json!(null));
        data.insert("tags".to_string(), json!(["a", "b"]));

        let result = values_to_string_map(&data);

        assert_eq!(result["title"], "Hello");
        assert_eq!(result["count"], "42");
        assert_eq!(result["active"], "true");
        assert_eq!(result["empty"], "");
        assert_eq!(result["tags"], r#"["a","b"]"#);
    }
}
