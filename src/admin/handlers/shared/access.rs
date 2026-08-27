//! Access control helpers — collection/global/field-level access checks.

use std::collections::HashMap;

use crate::hooks::{AccessCheckInput, ConditionContext};

use axum::{Extension, response::IntoResponse};
use serde_json::{Map, Value, json};
use tracing::{error, warn};

use crate::{
    admin::AdminState,
    core::{AuthUser, Document, DocumentFields, FieldDefinition, FieldDenial, FieldType, HookRef},
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
    access: Option<&HookRef>,
    auth_user: Option<&Extension<AuthUser>>,
    id: Option<&str>,
    data: Option<&DocumentFields>,
    operation: &str,
    collection: &str,
) -> Result<AccessResult, Box<axum::response::Response>> {
    if access.is_none() {
        return if state.config.access.default_deny {
            Ok(AccessResult::Denied)
        } else {
            Ok(AccessResult::Allowed)
        };
    }

    let user_doc = get_user_doc(auth_user);

    let mut conn = state
        .infra
        .pool
        .get()
        .map_err(|_| Box::new(forbidden(state, "Database error").into_response()))?;

    let tx = conn
        .transaction()
        .map_err(|_| Box::new(forbidden(state, "Database error").into_response()))?;

    let result = state
        .infra
        .hook_runner
        .check_access(
            &AccessCheckInput::builder(operation, collection)
                .access(access)
                .user(user_doc)
                .id(id)
                .data(data)
                .build(),
            &tx,
        )
        .inspect_err(|e| error!("Access check error: {}", e))
        .map_err(|_| Box::new(forbidden(state, "Access check failed").into_response()))?;

    tx.commit()
        .map_err(|_| Box::new(forbidden(state, "Database error").into_response()))?;

    Ok(result)
}

/// Returns the read-denied field paths for `document` (data-aware: each
/// `access.read` rule sees the document as `ctx.data` / `ctx.document`), or a
/// server-error response. Used by the edit forms to drop denied field inputs
/// from rendering. The service read already stripped denied *values*; this
/// yields the *names* the form must not render. Skips the check entirely
/// (returns empty vec) when no field configures read access.
pub fn compute_denied_read_fields(
    state: &AdminState,
    auth_user: Option<&Extension<AuthUser>>,
    fields: &[FieldDefinition],
    collection: &str,
    document: &DocumentFields,
) -> Result<Vec<FieldDenial>, Box<axum::response::Response>> {
    if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
        return Ok(Vec::new());
    }

    let user_doc = get_user_doc(auth_user);

    let mut conn = state
        .infra
        .pool
        .get()
        .inspect_err(|e| error!("Field access check pool error: {}", e))
        .map_err(|_| Box::new(server_error(state, "Database error")))?;

    let tx = conn
        .transaction()
        .inspect_err(|e| error!("Field access check tx error: {}", e))
        .map_err(|_| Box::new(server_error(state, "Database error")))?;

    let denied = state
        .infra
        .hook_runner
        .read_denied_names(fields, document, collection, user_doc, None, &tx);

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    Ok(denied)
}

/// Recursively collect all `admin.condition` hook refs from field definitions,
/// keyed by **field name** (matching the form's `data-field-name`) so the
/// evaluate-conditions endpoint resolves a client-sent field to *that field's*
/// configured [`HookRef`] — and thus its own `options`. Keying by field name
/// (not the ref string) is what lets two fields share a condition function with
/// different options; it also hardens the endpoint, since the server uses the
/// field's configured ref rather than trusting the client-sent ref string.
pub fn collect_condition_refs(fields: &[FieldDefinition]) -> HashMap<&str, &HookRef> {
    let mut refs = HashMap::new();

    for field in fields {
        if let Some(ref cond) = field.admin.condition {
            refs.insert(field.name.as_str(), cond);
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
    /// Whether the form is creating or editing — sent by the client so the
    /// condition's `ctx.operation` matches the live form. Defaults to
    /// `"update"` for older clients that don't send it.
    #[serde(default = "default_condition_operation")]
    pub operation: String,
}

fn default_condition_operation() -> String {
    "update".to_string()
}

/// Evaluate display conditions and return a `field_name` → visible map.
///
/// Validates each function ref against the set of known condition refs from the
/// field definitions to prevent calling arbitrary Lua functions.
pub fn evaluate_condition_results(
    hook_runner: &HookRunner,
    fields: &[FieldDefinition],
    req: &EvaluateConditionsRequest,
    cond_ctx: &ConditionContext<'_>,
) -> Map<String, Value> {
    use crate::hooks::DisplayConditionResult;

    let by_field = collect_condition_refs(fields);
    let form_data = json!(req.form_data);
    let mut results = Map::new();

    // Iterate the client-sent field names; the field's OWN configured ref is
    // resolved server-side (carrying its `options`) — the client-sent ref
    // string (the map value) is not trusted, so only the keys matter.
    for field_name in req.conditions.keys() {
        let Some(hook) = by_field.get(field_name.as_str()) else {
            warn!("evaluate_conditions: rejecting unknown condition field '{field_name}'");
            results.insert(field_name.clone(), json!(true));
            continue;
        };

        let visible = match hook_runner.call_display_condition(hook, &form_data, cond_ctx) {
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
/// Called without `id` / `data`. An access fn that returns a row **filter**
/// (`Constrained`) counts as visible here (`true`) — the user can act on
/// *some* rows, so the UI shows the entry/button. Only a fn that returns a
/// plain `false` (a content-gating check with no `id`/`data` to satisfy)
/// evaluates false. Either way the server-side enforcement re-runs the same
/// fn with full context when the user actually tries the action.
pub fn has_access_with_conn(
    state: &AdminState,
    access: Option<&HookRef>,
    user_doc: Option<&Document>,
    conn: &dyn crate::db::DbConnection,
    operation: &str,
    collection: &str,
) -> bool {
    if access.is_none() {
        return !state.config.access.default_deny;
    }

    let result = state.infra.hook_runner.check_access(
        &AccessCheckInput::builder(operation, collection)
            .access(access)
            .user(user_doc)
            .build(),
        conn,
    );

    matches!(
        result,
        Ok(AccessResult::Allowed | AccessResult::Constrained(_))
    )
}

/// Boolean access check for a single `operation`, opening its own transaction.
/// Backs the convenience wrappers below; use [`has_access_with_conn`] directly
/// when checking multiple permissions in a row (to share one transaction).
///
/// `collection` is the slug exposed to the access function as `ctx.collection`
/// (empty for slug-less custom pages, whose access rule has no collection).
fn has_op_access(
    state: &AdminState,
    access: Option<&HookRef>,
    user_doc: Option<&Document>,
    collection: &str,
    operation: &str,
) -> bool {
    if access.is_none() {
        return !state.config.access.default_deny;
    }

    let Ok(mut conn) = state.infra.pool.get() else {
        return false;
    };

    let Ok(tx) = conn.transaction() else {
        return false;
    };

    let allowed = has_access_with_conn(state, access, user_doc, &tx, operation, collection);

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    allowed
}

/// Quick read-access check for dashboard/list visibility (operation `"read"`).
pub fn has_read_access(
    state: &AdminState,
    access: Option<&HookRef>,
    user_doc: Option<&Document>,
    collection: &str,
) -> bool {
    has_op_access(state, access, user_doc, collection, "read")
}

/// Admin-UI visibility for a collection/global entry (nav + dashboard).
///
/// Shows the entry only if the viewer passes **both** `access.read` (can they
/// read it at all) **and** `access.admin` (the admin-UI gate). A `None` admin
/// rule is permissive — `read` alone decides. The admin rule is evaluated
/// under operation `"admin"` so a hook branching on `ctx.operation` sees the
/// same value the route middleware passes, keeping nav/dashboard visibility in
/// lockstep with the real gate.
pub fn is_admin_visible(
    state: &AdminState,
    read_access: Option<&HookRef>,
    admin_access: Option<&HookRef>,
    user_doc: Option<&Document>,
    collection: &str,
) -> bool {
    if !has_read_access(state, read_access, user_doc, collection) {
        return false;
    }

    match admin_access {
        None => true,
        Some(admin_ref) => has_op_access(state, Some(admin_ref), user_doc, collection, "admin"),
    }
}

/// [`is_admin_visible`] against an existing connection/transaction. Use this
/// when filtering many entries in one pass (e.g. the sidebar nav) so each check
/// shares one transaction instead of acquiring a pooled connection per entry.
pub fn is_admin_visible_with_conn(
    state: &AdminState,
    read_access: Option<&HookRef>,
    admin_access: Option<&HookRef>,
    user_doc: Option<&Document>,
    conn: &dyn crate::db::DbConnection,
    collection: &str,
) -> bool {
    if !has_access_with_conn(state, read_access, user_doc, conn, "read", collection) {
        return false;
    }

    match admin_access {
        None => true,
        Some(admin_ref) => {
            has_access_with_conn(state, Some(admin_ref), user_doc, conn, "admin", collection)
        }
    }
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
    fn collects_condition_refs_recursively_keyed_by_field() {
        let fields = vec![
            with_condition("a", "cond.show_a"),
            FieldDefinition::builder("plain", FieldType::Text).build(), // no condition
            FieldDefinition::builder("grp", FieldType::Group)
                .fields(vec![
                    with_condition("b", "cond.show_b"),
                    with_condition("c", "cond.show_a"), // same ref, distinct field
                ])
                .build(),
        ];

        let map = collect_condition_refs(&fields);
        // Two fields (`a`, `c`) share a ref but are kept as distinct keys; the
        // no-condition `plain` field is absent.
        assert_eq!(map.get("a").unwrap().reference(), "cond.show_a");
        assert_eq!(map.get("c").unwrap().reference(), "cond.show_a");
        let mut names: Vec<&str> = map.into_keys().collect();
        names.sort_unstable();
        assert_eq!(names, vec!["a", "b", "c"]);
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
        assert!(collect_condition_refs(&fields).contains_key("x"));
    }
}
