//! Access control checks executed within the Lua VM.

use anyhow::Result;
use mlua::{Lua, Value};
use std::collections::HashMap;

use crate::core::field::FieldDefinition;
use crate::core::Document;
use crate::db::query::{AccessResult, Filter, FilterClause, FilterOp};

use super::converters::{document_to_lua_table, lua_parse_filter_op};
use super::execution::resolve_hook_function;
use super::DefaultDeny;

/// Check collection-level access using an already-held `&Lua` reference.
/// Does NOT lock the VM or manage TxContext — caller must ensure those are set.
/// Returns Allowed if `access_ref` is None (no restriction configured).
pub(crate) fn check_access_with_lua(
    lua: &Lua,
    access_ref: Option<&str>,
    user: Option<&Document>,
    id: Option<&str>,
    data: Option<&HashMap<String, serde_json::Value>>,
) -> Result<AccessResult> {
    let func_ref = match access_ref {
        Some(r) => r,
        None => {
            // No access function configured — check if default-deny is enabled
            let deny = lua
                .app_data_ref::<DefaultDeny>()
                .map(|d| d.0)
                .unwrap_or(false);
            return Ok(if deny {
                AccessResult::Denied
            } else {
                AccessResult::Allowed
            });
        }
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
            data_table.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
        }
        ctx_table.set("data", data_table)?;
    }

    let result: Value = func.call(ctx_table)?;

    match result {
        Value::Boolean(true) => Ok(AccessResult::Allowed),
        Value::Boolean(false) | Value::Nil => Ok(AccessResult::Denied),
        Value::Table(tbl) => {
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
                    _ => {}
                }
            }
            Ok(AccessResult::Constrained(clauses))
        }
        _ => Ok(AccessResult::Denied),
    }
}

/// Check field-level read access using an already-held `&Lua` reference.
/// Returns a list of field names that should be stripped (denied fields).
pub(crate) fn check_field_read_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    user: Option<&Document>,
) -> Vec<String> {
    let mut denied = Vec::new();
    for field in fields {
        if let Some(ref read_ref) = field.access.read {
            match check_access_with_lua(lua, Some(read_ref), user, None, None) {
                Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {}
                Ok(AccessResult::Denied) => denied.push(field.name.clone()),
                Err(e) => {
                    tracing::warn!("field access check error for {}: {}", field.name, e);
                    denied.push(field.name.clone());
                }
            }
        }
    }
    denied
}

/// Check field-level write access using an already-held `&Lua` reference.
/// Returns a list of field names that should be stripped from the input.
pub(crate) fn check_field_write_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    user: Option<&Document>,
    operation: &str,
) -> Vec<String> {
    let mut denied = Vec::new();
    for field in fields {
        let access_ref = match operation {
            "create" => field.access.create.as_deref(),
            "update" => field.access.update.as_deref(),
            _ => None,
        };
        if let Some(ref_str) = access_ref {
            match check_access_with_lua(lua, Some(ref_str), user, None, None) {
                Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {}
                Ok(AccessResult::Denied) => denied.push(field.name.clone()),
                Err(e) => {
                    tracing::warn!("field write access check error for {}: {}", field.name, e);
                    denied.push(field.name.clone());
                }
            }
        }
    }
    denied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::DocumentBuilder;
    use crate::core::field::{FieldAccess, FieldType};
    use mlua::Lua;

    /// Set up a Lua VM with test access functions available via require/package.loaded.
    fn setup_lua() -> Lua {
        let lua = Lua::new();
        lua.load(
            r#"
            local access = {}

            function access.allow(ctx)
                return true
            end

            function access.deny(ctx)
                return false
            end

            function access.return_nil(ctx)
                return nil
            end

            function access.return_number(ctx)
                return 42
            end

            function access.constrained_string(ctx)
                return { status = "published" }
            end

            function access.constrained_integer(ctx)
                return { priority = 1 }
            end

            function access.constrained_number(ctx)
                return { score = 3.14 }
            end

            function access.constrained_ops(ctx)
                return { score = { greater_than = "50" } }
            end

            function access.constrained_multi_ops(ctx)
                return { score = { greater_than = "10", less_than = "100" } }
            end

            function access.constrained_ignore_bool(ctx)
                return { active = true }
            end

            function access.check_user(ctx)
                if ctx.user and ctx.user.role == "admin" then
                    return true
                end
                return false
            end

            function access.check_id(ctx)
                if ctx.id == "doc-123" then
                    return true
                end
                return false
            end

            function access.check_data(ctx)
                if ctx.data and ctx.data.title == "test" then
                    return true
                end
                return false
            end

            function access.throw_error(ctx)
                error("access check failed!")
            end

            package.loaded["test_access"] = access
        "#,
        )
        .exec()
        .unwrap();
        lua
    }

    fn make_field(name: &str, access: FieldAccess) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .access(access)
            .build()
    }

    fn make_user_doc(role: &str) -> Document {
        let mut fields = HashMap::new();
        fields.insert("role".to_string(), serde_json::json!(role));
        fields.insert("email".to_string(), serde_json::json!("user@test.com"));
        DocumentBuilder::new("user-1").fields(fields).build()
    }

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
    fn access_constrained_ignores_bool_values() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            Some("test_access.constrained_ignore_bool"),
            None,
            None,
            None,
        )
        .unwrap();
        // Boolean values in the constraint table hit the _ => {} branch — no clauses added
        match result {
            AccessResult::Constrained(clauses) => {
                assert!(clauses.is_empty());
            }
            _ => panic!("expected Constrained (empty)"),
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
        let mut data = HashMap::new();
        data.insert("title".to_string(), serde_json::json!("test"));
        let result = check_access_with_lua(
            &lua,
            Some("test_access.check_data"),
            None,
            None,
            Some(&data),
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Allowed));

        let mut bad_data = HashMap::new();
        bad_data.insert("title".to_string(), serde_json::json!("other"));
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
        let lua = setup_lua();
        let result = check_access_with_lua(&lua, Some("test_access.throw_error"), None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn access_invalid_ref_errors() {
        let lua = setup_lua();
        let result = check_access_with_lua(&lua, Some("nonexistent_module.func"), None, None, None);
        assert!(result.is_err());
    }

    // ── check_field_read_access_with_lua ────────────────────────────────

    #[test]
    fn field_read_no_access_config_allows_all() {
        let lua = setup_lua();
        let fields = vec![
            make_field("title", FieldAccess::default()),
            make_field("body", FieldAccess::default()),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None);
        assert!(denied.is_empty());
    }

    #[test]
    fn field_read_allowed_not_in_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                read: Some("test_access.allow".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_read_access_with_lua(&lua, &fields, None);
        assert!(denied.is_empty());
    }

    #[test]
    fn field_read_denied_in_list() {
        let lua = setup_lua();
        let fields = vec![
            make_field(
                "secret",
                FieldAccess {
                    read: Some("test_access.deny".to_string()),
                    ..Default::default()
                },
            ),
            make_field("title", FieldAccess::default()),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None);
        assert_eq!(denied, vec!["secret"]);
    }

    #[test]
    fn field_read_constrained_counts_as_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "status",
            FieldAccess {
                read: Some("test_access.constrained_string".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_read_access_with_lua(&lua, &fields, None);
        assert!(denied.is_empty());
    }

    #[test]
    fn field_read_error_counts_as_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "broken",
            FieldAccess {
                read: Some("test_access.throw_error".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_read_access_with_lua(&lua, &fields, None);
        assert_eq!(denied, vec!["broken"]);
    }

    #[test]
    fn field_read_mixed_access() {
        let lua = setup_lua();
        let fields = vec![
            make_field(
                "public",
                FieldAccess {
                    read: Some("test_access.allow".to_string()),
                    ..Default::default()
                },
            ),
            make_field(
                "secret",
                FieldAccess {
                    read: Some("test_access.deny".to_string()),
                    ..Default::default()
                },
            ),
            make_field("plain", FieldAccess::default()),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None);
        assert_eq!(denied, vec!["secret"]);
    }

    #[test]
    fn field_read_with_user_context() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "admin_only",
            FieldAccess {
                read: Some("test_access.check_user".to_string()),
                ..Default::default()
            },
        )];

        let admin = make_user_doc("admin");
        let denied = check_field_read_access_with_lua(&lua, &fields, Some(&admin));
        assert!(denied.is_empty());

        let viewer = make_user_doc("viewer");
        let denied = check_field_read_access_with_lua(&lua, &fields, Some(&viewer));
        assert_eq!(denied, vec!["admin_only"]);
    }

    // ── check_field_write_access_with_lua ───────────────────────────────

    #[test]
    fn field_write_no_access_config_allows_all() {
        let lua = setup_lua();
        let fields = vec![make_field("title", FieldAccess::default())];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, "create");
        assert!(denied.is_empty());
        let denied = check_field_write_access_with_lua(&lua, &fields, None, "update");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_create_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "locked",
            FieldAccess {
                create: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, "create");
        assert_eq!(denied, vec!["locked"]);
    }

    #[test]
    fn field_write_update_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "immutable",
            FieldAccess {
                update: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, "update");
        assert_eq!(denied, vec!["immutable"]);
    }

    #[test]
    fn field_write_create_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.allow".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, "create");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_unknown_operation_allows() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.deny".to_string()),
                update: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        // Unknown operation = None access_ref = allowed (no restriction)
        let denied = check_field_write_access_with_lua(&lua, &fields, None, "delete");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_error_counts_as_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "broken",
            FieldAccess {
                create: Some("test_access.throw_error".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, "create");
        assert_eq!(denied, vec!["broken"]);
    }

    #[test]
    fn field_write_create_vs_update_different_access() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "role",
            FieldAccess {
                create: Some("test_access.allow".to_string()),
                update: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, "create");
        assert!(denied.is_empty());

        let denied = check_field_write_access_with_lua(&lua, &fields, None, "update");
        assert_eq!(denied, vec!["role"]);
    }

    #[test]
    fn field_write_constrained_counts_as_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "status",
            FieldAccess {
                create: Some("test_access.constrained_string".to_string()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, "create");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_with_user_context() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "admin_only",
            FieldAccess {
                update: Some("test_access.check_user".to_string()),
                ..Default::default()
            },
        )];
        let admin = make_user_doc("admin");
        let denied = check_field_write_access_with_lua(&lua, &fields, Some(&admin), "update");
        assert!(denied.is_empty());

        let viewer = make_user_doc("viewer");
        let denied = check_field_write_access_with_lua(&lua, &fields, Some(&viewer), "update");
        assert_eq!(denied, vec!["admin_only"]);
    }
}
