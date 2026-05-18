//! Collection-level access checks. The Lua hook returns one of:
//! - `true` → Allowed
//! - `false`/`nil`/unexpected type → Denied
//! - `table` → Constrained (read-only WHERE filters merged into the query)

use anyhow::Result;
use mlua::{Lua, Value};
use tracing::warn;

use crate::{
    core::{Document, DocumentFields},
    db::{AccessResult, Filter, FilterClause, FilterOp},
    hooks::{
        lifecycle::{
            DefaultDeny,
            converters::{document_to_lua_table, lua_parse_filter_op},
            execution::resolve_hook_function,
        },
        lua_api,
    },
};

pub(crate) fn check_access_with_lua(
    lua: &Lua,
    access_ref: Option<&str>,
    user: Option<&Document>,
    id: Option<&str>,
    data: Option<&DocumentFields>,
) -> Result<AccessResult> {
    let Some(func_ref) = access_ref else {
        // No access function configured — check if default-deny is enabled
        let deny = lua.app_data_ref::<DefaultDeny>().is_some_and(|d| d.0);

        return Ok(if deny {
            AccessResult::Denied
        } else {
            AccessResult::Allowed
        });
    };

    let func = resolve_hook_function(lua, func_ref)?;

    // Build context table: { user = ..., id = ..., data = ... }
    let ctx_table = lua.create_table()?;

    if let Some(user_doc) = user {
        let user_table = document_to_lua_table(lua, user_doc)?;

        ctx_table.set("user", user_table)?;
    }

    if let Some(doc_id) = id {
        ctx_table.set("id", doc_id)?;
    }

    if let Some(doc_data) = data {
        let data_table = lua.create_table()?;

        for (k, v) in doc_data {
            data_table.set(k.as_str(), lua_api::json_to_lua(lua, v)?)?;
        }

        ctx_table.set("data", data_table)?;
    }

    // A Lua error inside the access function (e.g. typo, runtime
    // exception, called a nil method) is fail-safe: treat as
    // Denied and warn. Without this catch the anyhow::Error
    // bubbles out → `From<anyhow::Error> for ServiceError` wraps
    // it as Internal → gRPC clients see `Status::internal` and
    // retry. The correct behavior is the same "fail closed" as
    // the unexpected-type branch below.
    let result: Value = match func.call(ctx_table) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Access function '{}' raised an error, denying access: {}",
                func_ref, e
            );

            return Ok(AccessResult::Denied);
        }
    };

    match result {
        Value::Boolean(true) => Ok(AccessResult::Allowed),
        Value::Boolean(false) | Value::Nil => Ok(AccessResult::Denied),
        Value::Table(tbl) => parse_access_constraints(&tbl),
        other => {
            warn!(
                "Access function '{}' returned unexpected type '{}', denying access",
                func_ref,
                other.type_name()
            );

            Ok(AccessResult::Denied)
        }
    }
}

/// Parse an access constraint table into filter clauses.
fn parse_access_constraints(tbl: &mlua::Table) -> Result<AccessResult> {
    let mut clauses = Vec::new();

    for pair in tbl.pairs::<String, Value>() {
        let (field, value) = pair?;

        match value {
            Value::String(s) => {
                clauses.push(FilterClause::Single(Filter {
                    field,
                    op: FilterOp::Equals(s.to_str()?.to_string()),
                }));
            }
            Value::Integer(i) => {
                clauses.push(FilterClause::Single(Filter {
                    field,
                    op: FilterOp::Equals(i.to_string()),
                }));
            }
            Value::Number(n) => {
                clauses.push(FilterClause::Single(Filter {
                    field,
                    op: FilterOp::Equals(n.to_string()),
                }));
            }
            Value::Table(op_tbl) => {
                for op_pair in op_tbl.pairs::<String, Value>() {
                    let (op_name, op_val) = op_pair?;
                    let op = lua_parse_filter_op(&op_name, &op_val)?;

                    clauses.push(FilterClause::Single(Filter {
                        field: field.clone(),
                        op,
                    }));
                }
            }
            Value::Boolean(b) => {
                let val = if b { "1" } else { "0" };

                clauses.push(FilterClause::Single(Filter {
                    field,
                    op: FilterOp::Equals(val.to_string()),
                }));
            }
            _ => {
                warn!(
                    "Access constraint for field '{}': unsupported value type, denying",
                    field
                );

                return Ok(AccessResult::Denied);
            }
        }
    }

    Ok(AccessResult::Constrained(clauses))
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::super::test_helpers::*;
    use super::*;
    use serde_json::json;

    // ── check_access_with_lua ───────────────────────────────────────────

    #[test]
    fn access_none_ref_returns_allowed() {
        let lua = setup_lua();
        // No DefaultDeny in app_data = defaults to allow
        let result = check_access_with_lua(&lua, None, None, None, None).unwrap();
        assert!(matches!(result, AccessResult::Allowed));
    }

    #[test]
    fn access_none_ref_default_deny_false_returns_allowed() {
        let lua = setup_lua();
        lua.set_app_data(DefaultDeny(false));
        let result = check_access_with_lua(&lua, None, None, None, None).unwrap();
        assert!(matches!(result, AccessResult::Allowed));
    }

    #[test]
    fn access_none_ref_default_deny_true_returns_denied() {
        let lua = setup_lua();
        lua.set_app_data(DefaultDeny(true));
        let result = check_access_with_lua(&lua, None, None, None, None).unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_explicit_allow_overrides_default_deny() {
        let lua = setup_lua();
        lua.set_app_data(DefaultDeny(true));
        // When an access function IS defined and returns true, default-deny doesn't matter
        let result =
            check_access_with_lua(&lua, Some("test_access.allow"), None, None, None).unwrap();
        assert!(matches!(result, AccessResult::Allowed));
    }

    #[test]
    fn access_returns_true_is_allowed() {
        let lua = setup_lua();
        let result =
            check_access_with_lua(&lua, Some("test_access.allow"), None, None, None).unwrap();
        assert!(matches!(result, AccessResult::Allowed));
    }

    #[test]
    fn access_returns_false_is_denied() {
        let lua = setup_lua();
        let result =
            check_access_with_lua(&lua, Some("test_access.deny"), None, None, None).unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_returns_nil_is_denied() {
        let lua = setup_lua();
        let result =
            check_access_with_lua(&lua, Some("test_access.return_nil"), None, None, None).unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_returns_unexpected_type_is_denied() {
        let lua = setup_lua();
        let result =
            check_access_with_lua(&lua, Some("test_access.return_number"), None, None, None)
                .unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_constrained_string_value() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            Some("test_access.constrained_string"),
            None,
            None,
            None,
        )
        .unwrap();
        match result {
            AccessResult::Constrained(clauses) => {
                assert_eq!(clauses.len(), 1);
                match &clauses[0] {
                    FilterClause::Single(f) => {
                        assert_eq!(f.field, "status");
                        assert!(matches!(&f.op, FilterOp::Equals(v) if v == "published"));
                    }
                    _ => panic!("expected Single clause"),
                }
            }
            _ => panic!("expected Constrained"),
        }
    }

    #[test]
    fn access_constrained_integer_value() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            Some("test_access.constrained_integer"),
            None,
            None,
            None,
        )
        .unwrap();
        match result {
            AccessResult::Constrained(clauses) => {
                assert_eq!(clauses.len(), 1);
                match &clauses[0] {
                    FilterClause::Single(f) => {
                        assert_eq!(f.field, "priority");
                        assert!(matches!(&f.op, FilterOp::Equals(v) if v == "1"));
                    }
                    _ => panic!("expected Single clause"),
                }
            }
            _ => panic!("expected Constrained"),
        }
    }

    #[test]
    fn access_constrained_number_value() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            Some("test_access.constrained_number"),
            None,
            None,
            None,
        )
        .unwrap();
        match result {
            AccessResult::Constrained(clauses) => {
                assert_eq!(clauses.len(), 1);
                match &clauses[0] {
                    FilterClause::Single(f) => {
                        assert_eq!(f.field, "score");
                        assert!(matches!(&f.op, FilterOp::Equals(v) if v == "3.14"));
                    }
                    _ => panic!("expected Single clause"),
                }
            }
            _ => panic!("expected Constrained"),
        }
    }

    #[test]
    fn access_constrained_with_operator_table() {
        let lua = setup_lua();
        let result =
            check_access_with_lua(&lua, Some("test_access.constrained_ops"), None, None, None)
                .unwrap();
        match result {
            AccessResult::Constrained(clauses) => {
                assert_eq!(clauses.len(), 1);
                match &clauses[0] {
                    FilterClause::Single(f) => {
                        assert_eq!(f.field, "score");
                        assert!(matches!(&f.op, FilterOp::GreaterThan(v) if v == "50"));
                    }
                    _ => panic!("expected Single clause"),
                }
            }
            _ => panic!("expected Constrained"),
        }
    }

    #[test]
    fn access_constrained_multi_ops() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            Some("test_access.constrained_multi_ops"),
            None,
            None,
            None,
        )
        .unwrap();
        match result {
            AccessResult::Constrained(clauses) => {
                assert_eq!(clauses.len(), 2);
                // Both should be Single clauses for "score"
                for clause in &clauses {
                    match clause {
                        FilterClause::Single(f) => assert_eq!(f.field, "score"),
                        _ => panic!("expected Single clause"),
                    }
                }
            }
            _ => panic!("expected Constrained"),
        }
    }

    #[test]
    fn access_constrained_boolean_value() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            Some("test_access.constrained_ignore_bool"),
            None,
            None,
            None,
        )
        .unwrap();
        // Boolean values are converted to "1"/"0" filter constraints
        match result {
            AccessResult::Constrained(clauses) => {
                assert_eq!(clauses.len(), 1);
                match &clauses[0] {
                    FilterClause::Single(f) => {
                        assert_eq!(f.field, "active");
                        assert!(matches!(&f.op, FilterOp::Equals(v) if v == "1"));
                    }
                    _ => panic!("expected Single clause"),
                }
            }
            _ => panic!("expected Constrained"),
        }
    }

    #[test]
    fn access_passes_user_context() {
        let lua = setup_lua();
        let admin = make_user_doc("admin");
        let result = check_access_with_lua(
            &lua,
            Some("test_access.check_user"),
            Some(&admin),
            None,
            None,
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Allowed));

        let viewer = make_user_doc("viewer");
        let result = check_access_with_lua(
            &lua,
            Some("test_access.check_user"),
            Some(&viewer),
            None,
            None,
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_passes_no_user() {
        let lua = setup_lua();
        let result =
            check_access_with_lua(&lua, Some("test_access.check_user"), None, None, None).unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_passes_id_context() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            Some("test_access.check_id"),
            None,
            Some("doc-123"),
            None,
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Allowed));

        let result = check_access_with_lua(
            &lua,
            Some("test_access.check_id"),
            None,
            Some("doc-other"),
            None,
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_passes_data_context() {
        let lua = setup_lua();
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("test"));
        let result = check_access_with_lua(
            &lua,
            Some("test_access.check_data"),
            None,
            None,
            Some(&data),
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Allowed));

        let mut bad_data = DocumentFields::new();
        bad_data.insert("title".to_string(), json!("other"));
        let result = check_access_with_lua(
            &lua,
            Some("test_access.check_data"),
            None,
            None,
            Some(&bad_data),
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_error_propagates() {
        // Updated behavior: access fns that raise a Lua error are
        // treated as "denied" (fail-safe) and logged, rather than
        // propagating as anyhow::Error. The wire-level rationale is
        // documented in `grpc_hook_errors::access_fn_error_maps_to_permission_denied`.
        let lua = setup_lua();
        let result = check_access_with_lua(&lua, Some("test_access.throw_error"), None, None, None)
            .expect("error is caught + treated as Denied, not propagated");
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_invalid_ref_errors() {
        let lua = setup_lua();
        let result = check_access_with_lua(&lua, Some("nonexistent_module.func"), None, None, None);
        assert!(result.is_err());
    }

    // ── check_field_read_access_with_lua ────────────────────────────────
}
