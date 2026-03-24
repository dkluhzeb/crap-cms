//! Access control checks executed within the Lua VM.

use anyhow::Result;
use mlua::{Lua, Value};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

use crate::{
    core::{Document, FieldDefinition, FieldType},
    db::{AccessResult, Filter, FilterClause, FilterOp},
    hooks::{
        api,
        lifecycle::{
            DefaultDeny,
            converters::{document_to_lua_table, lua_parse_filter_op},
            execution::resolve_hook_function,
        },
    },
};

/// Check collection-level access using an already-held `&Lua` reference.
/// Does NOT lock the VM or manage TxContext — caller must ensure those are set.
/// Returns Allowed if `access_ref` is None (no restriction configured).
pub(crate) fn check_access_with_lua(
    lua: &Lua,
    access_ref: Option<&str>,
    user: Option<&Document>,
    id: Option<&str>,
    data: Option<&HashMap<String, JsonValue>>,
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
            data_table.set(k.as_str(), api::json_to_lua(lua, v)?)?;
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
                    Value::Boolean(b) => {
                        let val = if b { "1" } else { "0" };
                        clauses.push(FilterClause::Single(Filter {
                            field,
                            op: FilterOp::Equals(val.to_string()),
                        }));
                    }
                    _ => {
                        tracing::warn!(
                            "Access constraint for field '{}': unsupported value type, denying",
                            field
                        );
                        return Ok(AccessResult::Denied);
                    }
                }
            }
            Ok(AccessResult::Constrained(clauses))
        }
        _ => Ok(AccessResult::Denied),
    }
}

/// Check field-level read access using an already-held `&Lua` reference.
/// Returns a list of field names that should be stripped (denied fields).
/// Recurses into Group (with `__` prefix) and transparent layout containers (Row/Collapsible/Tabs).
pub(crate) fn check_field_read_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    user: Option<&Document>,
) -> Vec<String> {
    collect_field_access_denied(lua, fields, user, |f| f.access.read.as_deref(), "")
}

/// Check field-level write access using an already-held `&Lua` reference.
/// Returns a list of field names that should be stripped from the input.
/// Recurses into Group (with `__` prefix) and transparent layout containers (Row/Collapsible/Tabs).
pub(crate) fn check_field_write_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    user: Option<&Document>,
    operation: &str,
) -> Vec<String> {
    let extractor: fn(&FieldDefinition) -> Option<&str> = match operation {
        "create" => extract_create_access,
        "update" => extract_update_access,
        _ => return Vec::new(),
    };
    collect_field_access_denied(lua, fields, user, extractor, "")
}

fn extract_create_access(f: &FieldDefinition) -> Option<&str> {
    f.access.create.as_deref()
}

fn extract_update_access(f: &FieldDefinition) -> Option<&str> {
    f.access.update.as_deref()
}

/// Check whether any field (including nested sub-fields of Groups and transparent
/// containers) has an access function for the given extractor.
///
/// Mirrors `collect_field_access_denied`'s traversal pattern:
/// - Group: recurse into sub-fields.
/// - Row/Collapsible/Tabs: recurse (transparent containers).
/// - Array/Blocks: skip (separate join tables, no column-level stripping).
pub(crate) fn has_any_field_access(
    fields: &[FieldDefinition],
    extractor: fn(&FieldDefinition) -> Option<&str>,
) -> bool {
    for field in fields {
        if extractor(field).is_some() {
            return true;
        }
        if !field.fields.is_empty() {
            match field.field_type {
                FieldType::Group | FieldType::Row | FieldType::Collapsible | FieldType::Tabs => {
                    if has_any_field_access(&field.fields, extractor) {
                        return true;
                    }
                }
                _ => continue, // Array/Blocks — separate join tables
            }
        }
    }
    false
}

/// Recursively collect field names denied by an access check function.
///
/// - Group fields recurse with `parent__` prefix (matching DB column names).
/// - Row/Collapsible/Tabs are transparent — recurse with the same prefix.
/// - Array/Blocks have separate join tables and don't need column-level stripping.
fn collect_field_access_denied(
    lua: &Lua,
    fields: &[FieldDefinition],
    user: Option<&Document>,
    extractor: fn(&FieldDefinition) -> Option<&str>,
    prefix: &str,
) -> Vec<String> {
    let mut denied = Vec::new();
    for field in fields {
        let full_name = if prefix.is_empty() {
            field.name.clone()
        } else {
            format!("{}__{}", prefix, field.name)
        };

        if let Some(ref_str) = extractor(field) {
            match check_access_with_lua(lua, Some(ref_str), user, None, None) {
                Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {}
                _ => {
                    denied.push(full_name.clone());
                    continue; // Parent denied → skip sub-fields
                }
            }
        }

        // Recurse into containers with sub-fields
        if !field.fields.is_empty() {
            let sub_prefix = match field.field_type {
                FieldType::Group => &full_name,
                FieldType::Row | FieldType::Collapsible | FieldType::Tabs => prefix,
                _ => continue, // Array/Blocks don't need column-level stripping
            };
            denied.extend(collect_field_access_denied(
                lua,
                &field.fields,
                user,
                extractor,
                sub_prefix,
            ));
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
    use serde_json::json;

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
        fields.insert("role".to_string(), json!(role));
        fields.insert("email".to_string(), json!("user@test.com"));
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
        let mut data = HashMap::new();
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

        let mut bad_data = HashMap::new();
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

    // ── recursive field access ────────────────────────────────────────

    #[test]
    fn field_read_recurses_into_group_with_prefix() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![make_field(
                    "title",
                    FieldAccess {
                        read: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None);
        assert_eq!(denied, vec!["seo__title"]);
    }

    #[test]
    fn field_read_recurses_through_row_without_prefix() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![make_field(
                    "secret",
                    FieldAccess {
                        read: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, None);
        assert_eq!(denied, vec!["secret"]);
    }

    #[test]
    fn field_write_recurses_into_group() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![make_field(
                    "debug",
                    FieldAccess {
                        create: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_write_access_with_lua(&lua, &fields, None, "create");
        assert_eq!(denied, vec!["config__debug"]);
    }

    #[test]
    fn field_read_does_not_recurse_into_array() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![make_field(
                    "name",
                    FieldAccess {
                        read: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        // Array sub-fields have separate join tables — no column-level stripping
        let denied = check_field_read_access_with_lua(&lua, &fields, None);
        assert!(denied.is_empty());
    }

    // ── has_any_field_access ─────────────────────────────────────────

    #[test]
    fn has_any_no_access_configured() {
        let fields = vec![
            make_field("title", FieldAccess::default()),
            make_field("body", FieldAccess::default()),
        ];
        assert!(!has_any_field_access(&fields, |f| f.access.read.as_deref()));
    }

    #[test]
    fn has_any_top_level_read() {
        let fields = vec![make_field(
            "secret",
            FieldAccess {
                read: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_deref()));
    }

    #[test]
    fn has_any_nested_in_group() {
        // Group "seo" has no access, but sub-field "canonical_url" does.
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![make_field(
                    "canonical_url",
                    FieldAccess {
                        read: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_deref()));
    }

    #[test]
    fn has_any_nested_in_row() {
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![make_field(
                    "secret",
                    FieldAccess {
                        create: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f
            .access
            .create
            .as_deref()));
    }

    #[test]
    fn has_any_skips_array_sub_fields() {
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![make_field(
                    "name",
                    FieldAccess {
                        read: Some("test_access.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        // Array sub-fields have separate join tables — not included
        assert!(!has_any_field_access(&fields, |f| f.access.read.as_deref()));
    }

    #[test]
    fn has_any_deeply_nested_group_in_row() {
        // Row > Group > sub-field with access
        let fields = vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("grp", FieldType::Group)
                        .fields(vec![make_field(
                            "deep",
                            FieldAccess {
                                update: Some("test_access.deny".to_string()),
                                ..Default::default()
                            },
                        )])
                        .build(),
                ])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f
            .access
            .update
            .as_deref()));
    }

    #[test]
    fn has_any_write_checks_correct_extractor() {
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.deny".to_string()),
                ..Default::default()
            },
        )];
        // Has create access, but checking update should return false
        assert!(!has_any_field_access(&fields, |f| f
            .access
            .update
            .as_deref()));
        assert!(has_any_field_access(&fields, |f| f
            .access
            .create
            .as_deref()));
    }
}
