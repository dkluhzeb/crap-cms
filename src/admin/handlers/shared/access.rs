//! Access control helpers — collection/global/field-level access checks.

use std::collections::{HashMap, HashSet};

use axum::{Extension, response::IntoResponse};
use serde_json::{Map, Value, json};
use tracing::{error, warn};

use crate::{
    admin::AdminState,
    core::{AuthUser, Document, DocumentFields, FieldDefinition, FieldType},
    db::AccessResult,
    hooks::{HookRunner, lifecycle::access::has_any_field_access},
};

use super::response::{forbidden, server_error};

/// Extract the user document from `AuthUser` extension (for access checks).
#[must_use]
pub fn get_user_doc(auth_user: Option<&Extension<AuthUser>>) -> Option<&Document> {
    auth_user.map(|Extension(au)| &au.user_doc)
}

/// Helper to check collection/global-level access. Returns `AccessResult` or renders a 403 page.
pub fn check_access_or_forbid(
    state: &AdminState,
    access_ref: Option<&str>,
    auth_user: Option<&Extension<AuthUser>>,
    id: Option<&str>,
    data: Option<&DocumentFields>,
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
        .inspect_err(|e| error!("Access check error: {}", e))
        .map_err(|_| Box::new(forbidden(state, "Access check failed").into_response()))?;

    tx.commit()
        .map_err(|_| Box::new(forbidden(state, "Database error").into_response()))?;

    Ok(result)
}

/// Returns field names denied for the current user's read access, or a server error response.
/// Skips the check entirely (returns empty vec) if no field has read access configured.
pub fn compute_denied_read_fields(
    state: &AdminState,
    auth_user: Option<&Extension<AuthUser>>,
    fields: &[FieldDefinition],
) -> Result<Vec<String>, Box<axum::response::Response>> {
    if !has_any_field_access(fields, |f| f.access.read.as_deref()) {
        return Ok(Vec::new());
    }

    let user_doc = get_user_doc(auth_user);

    let mut conn = state
        .pool
        .get()
        .inspect_err(|e| error!("Field access check pool error: {}", e))
        .map_err(|_| Box::new(server_error(state, "Database error")))?;

    let tx = conn
        .transaction()
        .inspect_err(|e| error!("Field access check tx error: {}", e))
        .map_err(|_| Box::new(server_error(state, "Database error")))?;

    let denied = state
        .hook_runner
        .check_field_read_access(fields, user_doc, &tx);

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    Ok(denied)
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
    pub form_data: DocumentFields,
    /// Map of field names to their condition function references.
    pub conditions: HashMap<String, String>,
}

/// Evaluate display conditions and return a `field_name` → visible map.
///
/// Validates each function ref against the set of known condition refs from the
/// field definitions to prevent calling arbitrary Lua functions.
pub fn evaluate_condition_results(
    hook_runner: &HookRunner,
    fields: &[FieldDefinition],
    req: &EvaluateConditionsRequest,
) -> Map<String, Value> {
    use crate::hooks::DisplayConditionResult;

    let valid_refs = collect_condition_refs(fields);
    let form_data = json!(req.form_data);
    let mut results = Map::new();

    for (field_name, func_ref) in &req.conditions {
        if !valid_refs.contains(func_ref.as_str()) {
            warn!(
                "evaluate_conditions: rejecting unknown func_ref '{}' for field '{}'",
                func_ref, field_name,
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

/// Boolean access check — used for UI-gating ("can this user create X?")
/// rather than enforcement. Treats missing `access_ref` as allowed unless
/// `default_deny` is configured, in which case the no-config case denies.
///
/// Takes an existing connection so callers computing multiple permissions
/// (e.g. `CollectionPermissions::for_user`) can share a single transaction
/// instead of paying for a pool acquisition per check.
///
/// Called without `id` / `data`; access fns that gate on doc content
/// (rather than user role) will return false here, which errs on the safe
/// side for UI visibility — the server-side enforcement runs the same fn
/// with full context when the user actually tries the action.
pub fn has_access_with_conn(
    state: &AdminState,
    access_ref: Option<&str>,
    user_doc: Option<&Document>,
    conn: &dyn crate::db::DbConnection,
) -> bool {
    if access_ref.is_none() {
        return !state.config.access.default_deny;
    }

    let result = state
        .hook_runner
        .check_access(access_ref, user_doc, None, None, conn);

    matches!(
        result,
        Ok(AccessResult::Allowed | AccessResult::Constrained(_))
    )
}

/// Quick read-access check for dashboard/list visibility. Convenience
/// wrapper around [`has_access_with_conn`] that opens its own transaction.
/// Use the with-conn variant when checking multiple permissions in a row.
pub fn has_read_access(
    state: &AdminState,
    access_ref: Option<&str>,
    user_doc: Option<&Document>,
) -> bool {
    if access_ref.is_none() {
        return !state.config.access.default_deny;
    }

    let Ok(mut conn) = state.pool.get() else {
        return false;
    };

    let Ok(tx) = conn.transaction() else {
        return false;
    };

    let allowed = has_access_with_conn(state, access_ref, user_doc, &tx);

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    allowed
}

#[cfg(test)]
mod tests {
    use crate::core::field::{FieldAdmin, FieldTab, FieldType};

    use super::*;

    fn with_condition(name: &str, cond: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .admin(FieldAdmin::builder().condition(cond).build())
            .build()
    }

    #[test]
    fn collects_condition_refs_recursively_and_dedups() {
        let fields = vec![
            with_condition("a", "cond.show_a"),
            FieldDefinition::builder("plain", FieldType::Text).build(), // no condition
            FieldDefinition::builder("grp", FieldType::Group)
                .fields(vec![
                    with_condition("b", "cond.show_b"),
                    with_condition("c", "cond.show_a"), // duplicate ref
                ])
                .build(),
        ];

        let mut refs: Vec<&str> = collect_condition_refs(&fields).into_iter().collect();
        refs.sort_unstable();
        assert_eq!(refs, vec!["cond.show_a", "cond.show_b"]);
    }

    #[test]
    fn recurses_into_tabs() {
        let fields = vec![
            FieldDefinition::builder("tabs", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "T",
                    vec![with_condition("x", "cond.x")],
                )])
                .build(),
        ];
        assert!(collect_condition_refs(&fields).contains("cond.x"));
    }
}
