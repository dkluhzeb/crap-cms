//! Shared types and helpers for validation endpoints.

use std::collections::HashMap;

use axum::{
    Extension, Json,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::error;

use crate::{
    admin::{AdminState, Translations, handlers::shared::translate_validation_errors},
    core::{
        Document, FieldDefinition, auth::AuthUser, collection::Hooks, validate::ValidationError,
    },
    db::{DbPool, query::LocaleContext},
    hooks::HookRunner,
    service,
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

/// Quick error response for non-validation failures.
pub fn validation_error_response_simple(msg: &str) -> Response {
    Json(json!({
        "valid": false,
        "errors": { "_form": msg },
    }))
    .into_response()
}

/// Parameters for a validation run.
pub struct RunValidationParams<'a> {
    pub pool: &'a DbPool,
    pub runner: &'a HookRunner,
    pub hooks: &'a Hooks,
    pub fields: &'a [FieldDefinition],
    pub slug: &'a str,
    pub table_name: &'a str,
    pub operation: &'a str,
    pub exclude_id: Option<&'a str>,
    pub form_data: &'a HashMap<String, String>,
    pub join_data: &'a HashMap<String, Value>,
    pub is_draft: bool,
    pub soft_delete: bool,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub user_doc: Option<&'a Document>,
}

/// Run the before_validate → validate pipeline inside a rolled-back transaction.
///
/// Used by both collection and global validation endpoints. The `table_name`
/// parameter allows globals to pass `_global_{slug}` while collections pass
/// the collection slug directly.
pub fn run_validation(p: &RunValidationParams) -> anyhow::Result<()> {
    let mut conn = p.pool.get()?;
    let tx = conn.transaction()?;

    let locale = p.locale_ctx.and_then(|ctx| {
        if let crate::db::query::LocaleMode::Single(l) = &ctx.mode {
            Some(l.clone())
        } else {
            None
        }
    });

    let wh = service::RunnerWriteHooks::new(p.runner).with_conn(&tx);

    let input = service::WriteInput::builder(p.form_data.clone(), p.join_data)
        .locale_ctx(p.locale_ctx)
        .locale(locale)
        .draft(p.is_draft)
        .build();

    let validate_ctx = service::ValidateContext {
        slug: p.slug,
        table_name: p.table_name,
        fields: p.fields,
        hooks: p.hooks,
        operation: p.operation,
        exclude_id: p.exclude_id,
        soft_delete: p.soft_delete,
    };

    service::validate_document(&tx, &wh, &validate_ctx, input, p.user_doc)
        .map_err(|e| e.into_anyhow())?;

    // Always rollback — this is validation only
    drop(tx);

    Ok(())
}

/// Handle the result of a validation run, returning the appropriate JSON response.
pub fn handle_validation_result(
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
    auth_user: &Option<Extension<AuthUser>>,
    state: &AdminState,
) -> Response {
    match result {
        Ok(Ok(())) => validation_ok_response(),
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let locale = auth_user
                    .as_ref()
                    .map(|Extension(au)| au.ui_locale.as_str())
                    .unwrap_or("en");

                validation_error_response(ve, &state.translations, locale)
            } else {
                error!("Validation error: {:#}", e);
                validation_error_response_simple("Validation failed")
            }
        }
        Err(e) => {
            error!("Validate task error: {}", e);
            validation_error_response_simple("Internal error")
        }
    }
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
