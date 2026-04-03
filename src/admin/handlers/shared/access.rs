//! Access control helpers — collection/global/field-level access checks.

use std::collections::{HashMap, HashSet};

use axum::{Extension, response::IntoResponse};
use serde_json::{Map, Value, json};
use tracing::{error, warn};

use crate::{
    admin::AdminState,
    core::{AuthUser, Document, FieldDefinition, FieldType, event::EventUser},
    db::AccessResult,
    hooks::{HookRunner, lifecycle::access::has_any_field_access},
};

use super::response::{forbidden, server_error};

/// Extract the user document from AuthUser extension (for access checks).
pub fn get_user_doc(auth_user: &Option<Extension<AuthUser>>) -> Option<&Document> {
    auth_user.as_ref().map(|Extension(au)| &au.user_doc)
}

/// Extract an EventUser from the AuthUser extension (for SSE event attribution).
pub fn get_event_user(auth_user: &Option<Extension<AuthUser>>) -> Option<EventUser> {
    auth_user
        .as_ref()
        .map(|Extension(au)| EventUser::new(au.claims.sub.clone(), au.claims.email.clone()))
}

/// Strip denied fields from a document's fields map.
pub fn strip_denied_fields(fields: &mut HashMap<String, Value>, denied: &[String]) {
    for name in denied {
        fields.remove(name);
    }
}

/// Helper to check collection/global-level access. Returns AccessResult or renders a 403 page.
pub fn check_access_or_forbid(
    state: &AdminState,
    access_ref: Option<&str>,
    auth_user: &Option<Extension<AuthUser>>,
    id: Option<&str>,
    data: Option<&HashMap<String, Value>>,
) -> Result<AccessResult, Box<axum::response::Response>> {
    if access_ref.is_none() {
        return if state.config.access.default_deny {
            Ok(AccessResult::Denied)
        } else {
            Ok(AccessResult::Allowed)
        };
    }

    let user_doc = get_user_doc(auth_user);

    let mut conn = state
        .pool
        .get()
        .map_err(|_| Box::new(forbidden(state, "Database error").into_response()))?;

    let tx = conn
        .transaction()
        .map_err(|_| Box::new(forbidden(state, "Database error").into_response()))?;

    let result = state
        .hook_runner
        .check_access(access_ref, user_doc, id, data, &tx)
        .map_err(|e| {
            error!("Access check error: {}", e);
            Box::new(forbidden(state, "Access check failed").into_response())
        })?;

    tx.commit()
        .map_err(|_| Box::new(forbidden(state, "Database error").into_response()))?;

    Ok(result)
}

/// Returns field names denied for the current user's read access, or a server error response.
/// Skips the check entirely (returns empty vec) if no field has read access configured.
pub fn compute_denied_read_fields(
    state: &AdminState,
    auth_user: &Option<Extension<AuthUser>>,
    fields: &[FieldDefinition],
) -> Result<Vec<String>, Box<axum::response::Response>> {
    if !has_any_field_access(fields, |f| f.access.read.as_deref()) {
        return Ok(Vec::new());
    }

    let user_doc = get_user_doc(auth_user);

    let mut conn = state.pool.get().map_err(|e| {
        error!("Field access check pool error: {}", e);
        Box::new(server_error(state, "Database error"))
    })?;

    let tx = conn.transaction().map_err(|e| {
        error!("Field access check tx error: {}", e);
        Box::new(server_error(state, "Database error"))
    })?;

    let denied = state
        .hook_runner
        .check_field_read_access(fields, user_doc, &tx);

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    Ok(denied)
}

/// Strips fields denied for write access from a `HashMap<String, String>` form in-place.
/// Returns `Err(response)` on pool/tx failure, `Ok(())` on success.
pub fn strip_write_denied_string_fields(
    state: &AdminState,
    auth_user: &Option<Extension<AuthUser>>,
    fields: &[FieldDefinition],
    operation: &str,
    form_data: &mut HashMap<String, String>,
) -> Result<(), Box<axum::response::Response>> {
    let extractor: fn(&FieldDefinition) -> Option<&str> = match operation {
        "create" => |f| f.access.create.as_deref(),
        "update" => |f| f.access.update.as_deref(),
        _ => return Ok(()),
    };

    if !has_any_field_access(fields, extractor) {
        return Ok(());
    }

    let user_doc = get_user_doc(auth_user);

    let mut conn = state.pool.get().map_err(|e| {
        error!("Field access check pool error: {}", e);
        Box::new(server_error(state, "Database error"))
    })?;

    let tx = conn.transaction().map_err(|e| {
        error!("Field access check tx error: {}", e);
        Box::new(server_error(state, "Database error"))
    })?;

    let denied = state
        .hook_runner
        .check_field_write_access(fields, user_doc, operation, &tx);

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    for name in &denied {
        form_data.remove(name);
    }

    Ok(())
}

/// Recursively collect all `admin.condition` function refs from field definitions.
pub fn collect_condition_refs(fields: &[FieldDefinition]) -> HashSet<&str> {
    let mut refs = HashSet::new();

    for field in fields {
        if let Some(ref cond) = field.admin.condition {
            refs.insert(cond.as_str());
        }

        match field.field_type {
            FieldType::Group | FieldType::Row | FieldType::Collapsible => {
                refs.extend(collect_condition_refs(&field.fields));
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    refs.extend(collect_condition_refs(&tab.fields));
                }
            }
            _ => {}
        }
    }

    refs
}

/// Request payload for evaluating field display conditions.
/// Shared by collection and global evaluate-conditions endpoints.
#[derive(serde::Deserialize)]
pub struct EvaluateConditionsRequest {
    /// The current form data.
    pub form_data: HashMap<String, Value>,
    /// Map of field names to their condition function references.
    pub conditions: HashMap<String, String>,
}

/// Evaluate display conditions and return a field_name → visible map.
///
/// Validates each function ref against the set of known condition refs from the
/// field definitions to prevent calling arbitrary Lua functions.
pub fn evaluate_condition_results(
    hook_runner: &HookRunner,
    fields: &[FieldDefinition],
    req: &EvaluateConditionsRequest,
) -> Map<String, Value> {
    use crate::hooks::lifecycle::DisplayConditionResult;

    let valid_refs = collect_condition_refs(fields);
    let form_data = json!(req.form_data);
    let mut results = Map::new();

    for (field_name, func_ref) in &req.conditions {
        if !valid_refs.contains(func_ref.as_str()) {
            tracing::warn!(
                "evaluate_conditions: rejecting unknown func_ref '{}' for field '{}'",
                func_ref,
                field_name,
            );
            results.insert(field_name.clone(), json!(true));
            continue;
        }

        let visible = match hook_runner.call_display_condition(func_ref, &form_data) {
            Some(DisplayConditionResult::Bool(b)) => b,
            Some(DisplayConditionResult::Table { visible, .. }) => visible,
            None => true,
        };

        results.insert(field_name.clone(), json!(visible));
    }

    results
}

/// Quick read-access check for dashboard/list visibility.
/// Returns true if the user is allowed to see this collection or global.
pub fn has_read_access(
    state: &AdminState,
    access_ref: Option<&str>,
    user_doc: Option<&Document>,
) -> bool {
    if access_ref.is_none() {
        return !state.config.access.default_deny;
    }

    let mut conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };

    let tx = match conn.transaction() {
        Ok(t) => t,
        Err(_) => return false,
    };

    let result = state
        .hook_runner
        .check_access(access_ref, user_doc, None, None, &tx);

    if let Err(e) = tx.commit() {
        tracing::warn!("tx commit failed: {e}");
    }

    matches!(
        result,
        Ok(AccessResult::Allowed | AccessResult::Constrained(_))
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn strip_denied_fields_removes_specified_keys() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));
        fields.insert("secret".to_string(), json!("hidden"));
        fields.insert("body".to_string(), json!("content"));

        strip_denied_fields(&mut fields, &["secret".to_string()]);

        assert_eq!(fields.len(), 2);
        assert!(fields.contains_key("title"));
        assert!(fields.contains_key("body"));
        assert!(!fields.contains_key("secret"));
    }

    #[test]
    fn strip_denied_fields_empty_denied_list() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));
        fields.insert("body".to_string(), json!("content"));

        strip_denied_fields(&mut fields, &[]);
        assert_eq!(fields.len(), 2);
    }

    #[test]
    fn strip_denied_fields_empty_fields_map() {
        let mut fields: HashMap<String, Value> = HashMap::new();
        strip_denied_fields(&mut fields, &["secret".to_string()]);
        assert!(fields.is_empty());
    }

    #[test]
    fn strip_denied_fields_nonexistent_key() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));

        strip_denied_fields(&mut fields, &["nonexistent".to_string()]);
        assert_eq!(fields.len(), 1);
    }
}
