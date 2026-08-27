//! Collection-level access checks. The Lua hook returns one of:
//! - `true` → Allowed
//! - `false`/`nil`/unexpected type → Denied
//! - `table` → Constrained (read-only WHERE filters merged into the query)

use anyhow::Result;
use mlua::{Lua, LuaSerdeExt, Value};
use tracing::warn;

use crate::{
    db::{AccessResult, Filter, FilterClause, FilterOp},
    hooks::{
        lifecycle::{
            AccessCheckInput, AccessContext, LuaVmInfra, execution::resolve_hook_function,
        },
        lua_api::crud::filter::FilterValue,
    },
    service::{validate_access_constraint_locales, validate_access_constraints},
};

pub(crate) fn check_access_with_lua(
    lua: &Lua,
    input: &AccessCheckInput<'_>,
) -> Result<AccessResult> {
    let Some(hook) = input.access else {
        // No access function configured — check if default-deny is enabled
        let deny = lua
            .app_data_ref::<LuaVmInfra>()
            .is_some_and(|i| i.default_deny);

        return Ok(if deny {
            AccessResult::Denied
        } else {
            AccessResult::Allowed
        });
    };

    let func_ref = hook.reference();
    let func = resolve_hook_function(lua, func_ref)?;

    // Build the access-context table from a typed Rust struct (see
    // `hooks::lifecycle::AccessContext`) so the Lua shape is the single
    // source of truth.
    let ctx = AccessContext {
        user: input.user,
        id: input.id,
        data: input.data,
        document: input.document,
        locale: input.locale,
        operation: input.operation,
        collection: input.collection,
        ui_locale: input.ui_locale,
        options: hook.options(),
    };
    let ctx_table = lua.to_value(&ctx)?;

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
        Value::Table(tbl) => parse_access_constraints(lua, &tbl),
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

/// [`check_access_with_lua`] plus enforcement of the access-constraint contract
/// (equality/membership operators only; no system columns) on any `Constrained`
/// result.
///
/// This is the **single chokepoint** where a collection access hook's row
/// filters are validated before they leave the hooks layer — so every consumer
/// gets the same guarantee, not just the read filter-builder path. The two
/// in-memory enforcement surfaces (live event streams; relationship/join
/// population) resolve access through the `check_access` impls that call this
/// wrapper, so a hook that returns a `like`/ordered constraint can never reach
/// the in-memory matcher where SQL and in-memory evaluation could diverge.
/// [`check_access_with_lua`] itself stays a pure evaluator/parser.
///
/// # Errors
///
/// Propagates the validation error (a [`ServiceError::HookError`], surfaced as
/// `invalid_argument`) when a `Constrained` result uses a disallowed operator or
/// a system column.
///
/// [`ServiceError::HookError`]: crate::service::ServiceError::HookError
pub(crate) fn check_collection_access(
    lua: &Lua,
    input: &AccessCheckInput<'_>,
) -> Result<AccessResult> {
    let result = check_access_with_lua(lua, input)?;

    if let AccessResult::Constrained(filters) = &result {
        // Operator allowlist + system-column + dotted-path rules (context-free).
        validate_access_constraints(filters, false, false, input.collection)
            .map_err(anyhow::Error::new)?;

        // Locale-scoped-field rule (needs the collection's field types). The
        // registry snapshot is in the VM-stable infra bundle, so this fires
        // uniformly for every surface that resolves access through this
        // chokepoint — direct reads, Lua CRUD, relationship/join population,
        // and live event streams.
        if let Some(infra) = lua.app_data_ref::<LuaVmInfra>() {
            let registry = &infra.registry;
            let fields = registry
                .get_collection(input.collection)
                .map(|d| &d.fields)
                .or_else(|| registry.get_global(input.collection).map(|d| &d.fields));

            if let Some(fields) = fields {
                validate_access_constraint_locales(filters, fields, input.collection)
                    .map_err(anyhow::Error::new)?;
            }
        }
    }

    Ok(result)
}

/// Parse an access constraint table into filter clauses. Shares the
/// `FilterValue` parser with the user-CRUD path
/// (`hooks::lua_api::crud::filter`); the only access-specific
/// quirk is top-level booleans which bind as `"1"`/`"0"` for SQL
/// integer-boolean column compatibility (the CRUD path emits
/// `"true"`/`"false"` because that's what `serde_json` produces).
fn parse_access_constraints(lua: &Lua, tbl: &mlua::Table) -> Result<AccessResult> {
    let mut clauses = Vec::new();

    for pair in tbl.pairs::<String, Value>() {
        let (field, value) = pair?;

        // Boolean is a top-level access-filter quirk: SQL bind for
        // boolean (integer) columns expects `"1"`/`"0"`.
        if let Value::Boolean(b) = &value {
            clauses.push(FilterClause::Single(Filter {
                field,
                op: FilterOp::Equals(if *b { "1" } else { "0" }.to_string()),
            }));
            continue;
        }

        match FilterValue::from_lua_value(lua, &value) {
            Ok(fv) => {
                for op in fv.into_filter_ops() {
                    clauses.push(FilterClause::Single(Filter {
                        field: field.clone(),
                        op,
                    }));
                }
            }
            Err(e) => {
                warn!("Access constraint for field '{}': {}; denying", field, e);
                return Ok(AccessResult::Denied);
            }
        }
    }

    // Fail CLOSED on an empty constraint set. A hook that returned a *table*
    // intended to restrict which rows the caller sees, but it produced zero
    // filters — almost always because a nil-valued key was silently dropped by
    // Lua's table constructor: `{ tenant_id = ctx.user.tenant_id }` evaluates to
    // `{}` when `ctx.user.tenant_id` is nil (a tenantless user, a renamed field,
    // a partial signup). It can also happen via an empty operator table
    // (`{ score = {} }`). Treating that as `Constrained(vec![])` would AND
    // nothing into the query — i.e. match EVERY row — turning an
    // intended-to-restrict rule into a cross-tenant / cross-user data leak.
    // Allow-everything must be the explicit `return true`, never an empty table.
    if clauses.is_empty() {
        warn!(
            "Access function returned a constraint table that produced no filters \
             (likely a nil-valued key, e.g. `{{ field = ctx.user.<nil> }}`); \
             denying. Return `true` to allow unconditionally, or guard the nil \
             value explicitly."
        );
        return Ok(AccessResult::Denied);
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
    use crate::core::{DocumentFields, HookRef};
    use serde_json::json;

    /// Build an `AccessCheckInput` for tests (operation/collection are
    /// informational here).
    fn acc<'a>(
        access: Option<&'a HookRef>,
        data: Option<&'a DocumentFields>,
    ) -> AccessCheckInput<'a> {
        AccessCheckInput::builder("find", "test")
            .access(access)
            .data(data)
            .build()
    }

    // ── check_access_with_lua ───────────────────────────────────────────

    #[test]
    fn access_none_ref_returns_allowed() {
        let lua = setup_lua();
        // No LuaVmInfra in app_data = defaults to allow
        let result = check_access_with_lua(&lua, &acc(None, None)).unwrap();
        assert!(matches!(result, AccessResult::Allowed));
    }

    #[test]
    fn access_none_ref_default_deny_false_returns_allowed() {
        let lua = setup_lua();
        lua.set_app_data(LuaVmInfra::default());
        let result = check_access_with_lua(&lua, &acc(None, None)).unwrap();
        assert!(matches!(result, AccessResult::Allowed));
    }

    #[test]
    fn access_none_ref_default_deny_true_returns_denied() {
        let lua = setup_lua();
        lua.set_app_data(LuaVmInfra {
            default_deny: true,
            ..Default::default()
        });
        let result = check_access_with_lua(&lua, &acc(None, None)).unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_explicit_allow_overrides_default_deny() {
        let lua = setup_lua();
        lua.set_app_data(LuaVmInfra {
            default_deny: true,
            ..Default::default()
        });
        // When an access function IS defined and returns true, default-deny doesn't matter
        let result =
            check_access_with_lua(&lua, &acc(Some(&HookRef::new("test_access.allow")), None))
                .unwrap();
        assert!(matches!(result, AccessResult::Allowed));
    }

    #[test]
    fn access_returns_true_is_allowed() {
        let lua = setup_lua();
        let result =
            check_access_with_lua(&lua, &acc(Some(&HookRef::new("test_access.allow")), None))
                .unwrap();
        assert!(matches!(result, AccessResult::Allowed));
    }

    #[test]
    fn access_returns_false_is_denied() {
        let lua = setup_lua();
        let result =
            check_access_with_lua(&lua, &acc(Some(&HookRef::new("test_access.deny")), None))
                .unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_returns_nil_is_denied() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            &acc(Some(&HookRef::new("test_access.return_nil")), None),
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_returns_unexpected_type_is_denied() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            &acc(Some(&HookRef::new("test_access.return_number")), None),
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_constrained_string_value() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            &acc(Some(&HookRef::new("test_access.constrained_string")), None),
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
            &acc(Some(&HookRef::new("test_access.constrained_integer")), None),
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
            &acc(Some(&HookRef::new("test_access.constrained_number")), None),
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
        let result = check_access_with_lua(
            &lua,
            &acc(Some(&HookRef::new("test_access.constrained_ops")), None),
        )
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
            &acc(
                Some(&HookRef::new("test_access.constrained_multi_ops")),
                None,
            ),
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
            &acc(
                Some(&HookRef::new("test_access.constrained_ignore_bool")),
                None,
            ),
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

    /// CRITICAL regression: an access function that returns an EMPTY table must
    /// fail closed (`Denied`), never `Constrained(vec![])`. An empty constraint
    /// AND's nothing into the query — i.e. matches every row — so treating it as
    /// "allowed, unconstrained" turns an intended-to-restrict rule into a
    /// cross-tenant/cross-user data leak. Allow-all must be `return true`.
    #[test]
    fn access_empty_constraint_table_denies() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            &acc(Some(&HookRef::new("test_access.return_empty_table")), None),
        )
        .unwrap();
        assert!(
            matches!(result, AccessResult::Denied),
            "empty constraint table must deny, got {result:?}"
        );
    }

    /// CRITICAL regression: the canonical multi-tenant footgun. A hook returning
    /// `{ tenant_id = ctx.user.tenant_id }` where `tenant_id` is nil is `{}` at
    /// runtime (Lua drops nil-valued keys) — it must deny, not leak every tenant.
    #[test]
    fn access_nil_keyed_constraint_denies() {
        let lua = setup_lua();
        // user doc with NO tenant_id field → the constraint key evaporates.
        let user = make_user_doc("member");
        let result = check_access_with_lua(
            &lua,
            &AccessCheckInput::builder("find", "test")
                .access(Some(&HookRef::new("test_access.nil_keyed_constraint")))
                .user(Some(&user))
                .build(),
        )
        .unwrap();
        assert!(
            matches!(result, AccessResult::Denied),
            "nil-keyed constraint must deny (fail closed), got {result:?}"
        );
    }

    /// A non-empty table whose value parses to zero filter operators
    /// (`{ score = {} }`) also yields no filters → must fail closed.
    #[test]
    fn access_empty_operator_table_denies() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            &acc(
                Some(&HookRef::new("test_access.empty_operator_table")),
                None,
            ),
        )
        .unwrap();
        assert!(
            matches!(result, AccessResult::Denied),
            "constraint with no resulting operators must deny, got {result:?}"
        );
    }

    #[test]
    fn access_passes_user_context() {
        let lua = setup_lua();
        let admin = make_user_doc("admin");
        let result = check_access_with_lua(
            &lua,
            &AccessCheckInput::builder("find", "test")
                .access(Some(&HookRef::new("test_access.check_user")))
                .user(Some(&admin))
                .build(),
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Allowed));

        let viewer = make_user_doc("viewer");
        let result = check_access_with_lua(
            &lua,
            &AccessCheckInput::builder("find", "test")
                .access(Some(&HookRef::new("test_access.check_user")))
                .user(Some(&viewer))
                .build(),
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_passes_no_user() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            &acc(Some(&HookRef::new("test_access.check_user")), None),
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_passes_id_context() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            &AccessCheckInput::builder("find", "test")
                .access(Some(&HookRef::new("test_access.check_id")))
                .id(Some("doc-123"))
                .build(),
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Allowed));

        let result = check_access_with_lua(
            &lua,
            &AccessCheckInput::builder("find", "test")
                .access(Some(&HookRef::new("test_access.check_id")))
                .id(Some("doc-other"))
                .build(),
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
            &acc(Some(&HookRef::new("test_access.check_data")), Some(&data)),
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Allowed));

        let mut bad_data = DocumentFields::new();
        bad_data.insert("title".to_string(), json!("other"));
        let result = check_access_with_lua(
            &lua,
            &acc(
                Some(&HookRef::new("test_access.check_data")),
                Some(&bad_data),
            ),
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
        let result = check_access_with_lua(
            &lua,
            &acc(Some(&HookRef::new("test_access.throw_error")), None),
        )
        .expect("error is caught + treated as Denied, not propagated");
        assert!(matches!(result, AccessResult::Denied));
    }

    #[test]
    fn access_invalid_ref_errors() {
        let lua = setup_lua();
        let result = check_access_with_lua(
            &lua,
            &acc(Some(&HookRef::new("nonexistent_module.func")), None),
        );
        assert!(result.is_err());
    }

    /// Locale-aware access: the operation locale is exposed as `context.locale`
    /// so access functions can restrict by locale (e.g. a per-language editor).
    #[test]
    fn access_sees_locale_in_context() {
        let lua = setup_lua();

        // `access.check_locale` allows only when `ctx.locale == "en"`.
        let allowed = check_access_with_lua(
            &lua,
            &AccessCheckInput::builder("find", "test")
                .access(Some(&HookRef::new("test_access.check_locale")))
                .locale(Some("en"))
                .build(),
        )
        .unwrap();
        assert!(matches!(allowed, AccessResult::Allowed));

        let denied = check_access_with_lua(
            &lua,
            &AccessCheckInput::builder("find", "test")
                .access(Some(&HookRef::new("test_access.check_locale")))
                .locale(Some("de"))
                .build(),
        )
        .unwrap();
        assert!(matches!(denied, AccessResult::Denied));

        // No locale (localization disabled) → `ctx.locale` is nil → denied.
        let nil_locale = check_access_with_lua(
            &lua,
            &acc(Some(&HookRef::new("test_access.check_locale")), None),
        )
        .unwrap();
        assert!(matches!(nil_locale, AccessResult::Denied));
    }

    // ── check_collection_access (operator allowlist chokepoint) ──────────

    /// Regression: the single chokepoint rejects a `Constrained` result whose
    /// row filter uses a disallowed (ordered) operator, so it can never reach
    /// the in-memory matcher on the live-event or population surfaces. The bare
    /// parser ([`check_access_with_lua`]) still produces the clause; the
    /// wrapper is what enforces the contract.
    #[test]
    fn check_collection_access_rejects_disallowed_operator() {
        let lua = setup_lua();
        let err = check_collection_access(
            &lua,
            &acc(Some(&HookRef::new("test_access.constrained_ops")), None),
        )
        .unwrap_err();
        assert!(err.to_string().contains("greater_than"), "{err}");
    }

    /// An equality constraint passes the chokepoint unchanged — the allowed shape.
    #[test]
    fn check_collection_access_allows_equality_constraint() {
        let lua = setup_lua();
        let result = check_collection_access(
            &lua,
            &acc(Some(&HookRef::new("test_access.constrained_string")), None),
        )
        .unwrap();
        assert!(matches!(result, AccessResult::Constrained(_)));
    }

    /// `Allowed` / `Denied` results carry no filters and pass through untouched.
    #[test]
    fn check_collection_access_passes_through_non_constrained() {
        let lua = setup_lua();
        let allowed =
            check_collection_access(&lua, &acc(Some(&HookRef::new("test_access.allow")), None))
                .unwrap();
        assert!(matches!(allowed, AccessResult::Allowed));
    }

    // ── check_field_read_access_with_lua ────────────────────────────────
}
