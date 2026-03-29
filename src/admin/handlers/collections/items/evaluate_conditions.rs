use std::collections::HashMap;

use axum::{
    Extension, Json,
    extract::{Path, State},
    response::IntoResponse,
};
use serde_json::{Map, Value, json};

use crate::{
    admin::{AdminState, handlers::shared::check_access_or_forbid},
    core::auth::AuthUser,
    db::AccessResult,
    hooks::lifecycle::DisplayConditionResult,
};

/// POST /admin/collections/{slug}/evaluate-conditions
/// Evaluates server-only display conditions with current form data.
/// Returns JSON: { "field_name": true/false, ... }
pub async fn evaluate_conditions(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Json(req): Json<EvaluateConditionsRequest>,
) -> impl IntoResponse {
    // Check collection-level read access
    if let Some(def) = state.registry.get_collection(&slug) {
        match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
            Ok(AccessResult::Denied) => return Json(json!({})).into_response(),
            Err(_) => return Json(json!({})).into_response(),
            _ => {}
        }
    }

    // Collect the set of valid condition refs from the collection's field definitions
    // to prevent calling arbitrary Lua functions.
    let valid_refs: std::collections::HashSet<&str> =
        if let Some(def) = state.registry.get_collection(&slug) {
            collect_condition_refs(&def.fields)
        } else {
            return Json(json!({})).into_response();
        };

    let form_data = json!(req.form_data);
    let mut results = Map::new();

    for (field_name, func_ref) in &req.conditions {
        // Only evaluate function refs that are actually configured as display conditions
        if !valid_refs.contains(func_ref.as_str()) {
            tracing::warn!(
                "evaluate_conditions: rejecting unknown func_ref '{}' for field '{}'",
                func_ref,
                field_name
            );
            results.insert(field_name.clone(), json!(true));
            continue;
        }

        let visible = match state
            .hook_runner
            .call_display_condition(func_ref, &form_data)
        {
            Some(DisplayConditionResult::Bool(b)) => b,
            Some(DisplayConditionResult::Table { visible, .. }) => visible,
            None => true, // error -> show
        };

        results.insert(field_name.clone(), json!(visible));
    }

    Json(Value::Object(results)).into_response()
}

/// Request payload for evaluating field display conditions.
#[derive(serde::Deserialize)]
pub struct EvaluateConditionsRequest {
    /// The current form data.
    pub form_data: HashMap<String, Value>,
    /// Map of field names to their condition function references.
    pub conditions: HashMap<String, String>,
}

/// Recursively collect all `admin.condition` function refs from field definitions.
fn collect_condition_refs(
    fields: &[crate::core::FieldDefinition],
) -> std::collections::HashSet<&str> {
    let mut refs = std::collections::HashSet::new();

    for field in fields {
        if let Some(ref cond) = field.admin.condition {
            refs.insert(cond.as_str());
        }

        match field.field_type {
            crate::core::FieldType::Group
            | crate::core::FieldType::Row
            | crate::core::FieldType::Collapsible => {
                refs.extend(collect_condition_refs(&field.fields));
            }
            crate::core::FieldType::Tabs => {
                for tab in &field.tabs {
                    refs.extend(collect_condition_refs(&tab.fields));
                }
            }
            _ => {}
        }
    }

    refs
}
