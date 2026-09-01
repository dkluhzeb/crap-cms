//! Shared filter-input primitives for the CRUD `*QueryInput` types
//! (which live next to their respective handlers in `crud/collection/`).
//!
//! The Lua user writes `where = { field = "value", other = { contains
//! = "x" }, ["or"] = { {...}, ... } }`. `mlua`'s `LuaSerdeExt` first
//! deserializes the input into the handler's `*QueryInput` struct,
//! which holds the raw `where` map as
//! `Option<HashMap<String, serde_json::Value>>`. Each handler then
//! calls [`convert_where_clause`] to project that map into a runtime
//! [`Vec<FilterClause>`] — the per-key dispatch
//! ([`FilterValue::from_serde`] for normal keys, OR-group handling
//! for the special `"or"` key) lives here so all four handlers share
//! it.
//!
//! [`FilterValue::from_lua_value`] is the lower-level entry point used
//! by the access-filter path (`hooks::lifecycle::access::collection`),
//! which receives live `mlua::Value`s from access hooks rather than
//! going through `LuaSerdeExt::from_value`.

use std::collections::HashMap;

use mlua::{Error::RuntimeError, Lua, LuaSerdeExt, Result as LuaResult, Value};
use serde::Deserialize;

use crate::db::{FilterClause, FilterOp, query::filter::decode_where_map};
use crate::typegen::lua::{LuaAlias, LuaAnnotation, LuaTypeAlias};

/// Scalar filter value — `string`, `integer`, `number`, or `boolean`.
/// Modeled as an untagged Rust enum so a Lua string and a Lua number
/// both deserialize into the right variant without any per-field
/// hint. The Lua alias is emitted as a type-union derived from the
/// variant payload types (`boolean | integer | number | string`).
#[derive(Debug, Clone, Deserialize, LuaAlias)]
#[serde(untagged)]
#[lua(alias = "crap.FilterScalar")]
pub(crate) enum FilterScalar {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl FilterScalar {
    fn to_filter_string(&self) -> String {
        match self {
            Self::Bool(b) => b.to_string(),
            Self::Int(i) => i.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Str(s) => s.clone(),
        }
    }
}

/// Filter operator table. Use one key per operator on the operator
/// side of a `where = { field = { … } }` entry. Simple string /
/// number / boolean values on the right-hand side are treated as
/// `equals` automatically (see `FilterValue::Scalar`).
#[derive(Debug, Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.FilterOperators")]
pub(crate) struct FilterOperators {
    /// Exact match (`field = value`).
    #[lua(ty = "crap.FilterScalar", optional)]
    pub(crate) equals: Option<FilterScalar>,
    /// Not equal (`field != value`).
    #[lua(ty = "crap.FilterScalar", optional)]
    pub(crate) not_equals: Option<FilterScalar>,
    /// SQL `LIKE` pattern (`field LIKE value`).
    #[lua(optional)]
    pub(crate) like: Option<String>,
    /// Substring match (`field LIKE %value%`).
    #[lua(optional)]
    pub(crate) contains: Option<String>,
    /// Greater than (`field > value`).
    #[lua(ty = "crap.FilterScalar", optional)]
    pub(crate) greater_than: Option<FilterScalar>,
    /// Less than (`field < value`).
    #[lua(ty = "crap.FilterScalar", optional)]
    pub(crate) less_than: Option<FilterScalar>,
    /// Greater than or equal (`field >= value`).
    #[lua(ty = "crap.FilterScalar", optional)]
    pub(crate) greater_than_or_equal: Option<FilterScalar>,
    /// Less than or equal (`field <= value`).
    #[lua(ty = "crap.FilterScalar", optional)]
    pub(crate) less_than_or_equal: Option<FilterScalar>,
    /// Value in list (`field IN (...)`).
    #[serde(rename = "in")]
    #[lua(rename = "[\"in\"]", ty = "crap.FilterScalar[]", optional)]
    pub(crate) in_: Option<Vec<FilterScalar>>,
    /// Value not in list (`field NOT IN (...)`).
    #[lua(ty = "crap.FilterScalar[]", optional)]
    pub(crate) not_in: Option<Vec<FilterScalar>>,
    /// Field is not null (`IS NOT NULL`). Only `true` is accepted — `false` is an error.
    #[lua(optional)]
    pub(crate) exists: Option<bool>,
    /// Field is null (`IS NULL`). Only `true` is accepted — `false` is an error.
    #[lua(optional)]
    pub(crate) not_exists: Option<bool>,
}

impl FilterOperators {
    /// Expand the active operator slots into `FilterOp`s.
    ///
    /// # Errors
    ///
    /// `exists = false` / `not_exists = false` is an error, never a silently
    /// dropped slot: a dropped slot widens the match set (dangerous on
    /// `delete_many` / `update_many` and in access constraints), and the wire
    /// decoder (`db::query::filter::decode`) applies the identical rule.
    fn into_filter_ops(self) -> LuaResult<Vec<FilterOp>> {
        let mut out = Vec::new();
        if let Some(v) = self.equals {
            out.push(FilterOp::Equals(v.to_filter_string()));
        }
        if let Some(v) = self.not_equals {
            out.push(FilterOp::NotEquals(v.to_filter_string()));
        }
        if let Some(v) = self.like {
            out.push(FilterOp::Like(v));
        }
        if let Some(v) = self.contains {
            out.push(FilterOp::Contains(v));
        }
        if let Some(v) = self.greater_than {
            out.push(FilterOp::GreaterThan(v.to_filter_string()));
        }
        if let Some(v) = self.less_than {
            out.push(FilterOp::LessThan(v.to_filter_string()));
        }
        if let Some(v) = self.greater_than_or_equal {
            out.push(FilterOp::GreaterThanOrEqual(v.to_filter_string()));
        }
        if let Some(v) = self.less_than_or_equal {
            out.push(FilterOp::LessThanOrEqual(v.to_filter_string()));
        }
        if let Some(vs) = self.in_ {
            out.push(FilterOp::In(
                vs.iter().map(FilterScalar::to_filter_string).collect(),
            ));
        }
        if let Some(vs) = self.not_in {
            out.push(FilterOp::NotIn(
                vs.iter().map(FilterScalar::to_filter_string).collect(),
            ));
        }
        if let Some(v) = self.exists {
            out.push(exists_op("exists", v, FilterOp::Exists)?);
        }
        if let Some(v) = self.not_exists {
            out.push(exists_op("not_exists", v, FilterOp::NotExists)?);
        }

        Ok(out)
    }
}

/// `exists` / `not_exists` accept only `true` — see
/// [`FilterOperators::into_filter_ops`].
fn exists_op(name: &str, value: bool, op: FilterOp) -> LuaResult<FilterOp> {
    if value {
        return Ok(op);
    }

    Err(RuntimeError(format!(
        "filter operator '{name}' takes only `true` (got false); use `not_exists = true` for IS NULL and `exists = true` for IS NOT NULL"
    )))
}

/// One value in a `where` map: either a scalar (implicit `equals`) or
/// an operator table. Hand-rolled rather than derived because serde's
/// `untagged` macro can't disambiguate scalar variants of differing
/// type from each other before falling back to the struct branch;
/// going through `serde_json::Value` lets us be explicit. The
/// `Operators` variant is boxed because it's ~10× larger than the
/// scalar variant.
#[derive(Debug)]
pub(crate) enum FilterValue {
    /// Scalar value — treated as `equals`.
    Scalar(FilterScalar),
    /// Operator table — one entry per active operator.
    Operators(Box<FilterOperators>),
}

impl FilterValue {
    /// Build a `FilterValue` directly from an `mlua::Value`. Used by
    /// the access-filter path (`hooks::lifecycle::access::collection`)
    /// which walks an mlua `Table` returned by an access hook. The
    /// access path bypasses the Lua-→-JSON conversion since access
    /// hooks return live mlua values rather than going through serde.
    pub(crate) fn from_lua_value(lua: &Lua, value: &Value) -> LuaResult<Self> {
        match value {
            Value::String(s) => Ok(Self::Scalar(FilterScalar::Str(s.to_str()?.to_string()))),
            Value::Integer(i) => Ok(Self::Scalar(FilterScalar::Int(*i))),
            Value::Number(n) => Ok(Self::Scalar(FilterScalar::Float(*n))),
            Value::Boolean(b) => Ok(Self::Scalar(FilterScalar::Bool(*b))),
            Value::Table(_) => {
                let ops: FilterOperators = lua.from_value(value.clone())?;
                Ok(Self::Operators(Box::new(ops)))
            }
            other => Err(RuntimeError(format!(
                "filter value: unsupported Lua type '{}'",
                other.type_name()
            ))),
        }
    }

    /// Flatten this filter value into one or more `FilterOp`s. A
    /// scalar becomes a single `Equals` op; an operator table becomes
    /// one op per active operator slot.
    ///
    /// # Errors
    ///
    /// Propagates the `exists = false` / `not_exists = false` rejection from
    /// [`FilterOperators::into_filter_ops`].
    pub(crate) fn into_filter_ops(self) -> LuaResult<Vec<FilterOp>> {
        match self {
            Self::Scalar(s) => Ok(vec![FilterOp::Equals(s.to_filter_string())]),
            Self::Operators(ops) => (*ops).into_filter_ops(),
        }
    }
}

impl LuaAlias for FilterValue {
    const ALIAS_NAME: &'static str = "crap.FilterValue";

    fn render_lua_alias(out: &mut String) {
        out.push_str(
            "--- One filter value in a `where` clause: scalar (treated as\n\
             --- `equals`) or operator table.\n\
             --- @alias crap.FilterValue crap.FilterScalar | crap.FilterOperators\n\n",
        );
    }
}

/// One AND-group inside a `where.or` array. The Lua user passes a
/// map of `field → FilterValue` exactly like the top-level `where`
/// clause, and the OR clause is `{ group1, group2, … }`. Documented
/// as a type alias so the `where` type union (on each `*QueryInput`)
/// can name it.
#[derive(LuaTypeAlias)]
#[lua(alias = "crap.OrCondition", target = "table<string, crap.FilterValue>")]
pub(crate) struct OrCondition;

/// Pull a `where` `HashMap` (post-`from_value` JSON) into `Vec<FilterClause>`.
/// The `"or"` key is treated specially — its value must be an array of
/// AND-groups, each of which is a `where`-shaped map. Called by every
/// `*QueryInput::into_find_query` to convert the typed `where_` field
/// into runtime `FilterClause`s.
pub(crate) fn convert_where_clause(
    where_: HashMap<String, serde_json::Value>,
) -> LuaResult<Vec<FilterClause>> {
    // The canonical shared grammar (scalar shorthand, operator tables, `or`
    // groups) — identical to gRPC and MCP by construction. `FilterValue`
    // below stays for the access-constraint path, which walks live mlua
    // tables instead of serde values.
    let map: serde_json::Map<String, serde_json::Value> = where_.into_iter().collect();

    decode_where_map(&map).map_err(|e| RuntimeError(format!("invalid where clause: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scalar_eq(op: &FilterOp, expected: &str) -> bool {
        matches!(op, FilterOp::Equals(v) if v == expected)
    }

    // ── convert_where_clause (the CRUD where codec, over the shared
    // decode_where_map grammar) ─────────────────────────────────────

    fn one(field_value: serde_json::Value) -> Result<Vec<FilterClause>, mlua::Error> {
        let mut map = HashMap::new();
        map.insert("f".to_string(), field_value);
        convert_where_clause(map)
    }

    #[test]
    fn where_scalar_shorthand_becomes_equals() {
        for (v, expected) in [
            (json!(true), "true"),
            (json!(42), "42"),
            (json!(3.5), "3.5"),
            (json!("published"), "published"),
        ] {
            let clauses = one(v).unwrap();
            let FilterClause::Single(f) = &clauses[0] else {
                panic!("expected Single");
            };
            assert!(scalar_eq(&f.op, expected), "for {expected}");
        }
    }

    #[test]
    fn where_operator_table_decodes() {
        let clauses = one(json!({ "contains": "rust" })).unwrap();
        let FilterClause::Single(f) = &clauses[0] else {
            panic!("expected Single");
        };
        assert!(matches!(&f.op, FilterOp::Contains(v) if v == "rust"));
    }

    #[test]
    fn where_or_group_decodes() {
        let mut map = HashMap::new();
        map.insert(
            "or".to_string(),
            json!([{ "status": "a" }, { "status": "b" }]),
        );
        let clauses = convert_where_clause(map).unwrap();
        assert!(matches!(&clauses[0], FilterClause::Or(_)));
    }

    #[test]
    fn where_null_and_array_error() {
        assert!(one(json!(null)).is_err());
        assert!(one(json!(["a", "b"])).is_err());
    }

    #[test]
    fn where_unknown_operator_errors() {
        let err = one(json!({ "bad_op": "x" })).unwrap_err();
        assert!(err.to_string().contains("bad_op"), "got: {err}");
    }

    // ── FilterOperators → FilterOp expansion ────────────────────────

    fn ops_from(json_obj: serde_json::Value) -> Vec<FilterOp> {
        let ops: FilterOperators = serde_json::from_value(json_obj).unwrap();
        ops.into_filter_ops().unwrap()
    }

    #[test]
    fn operators_equals() {
        let ops = ops_from(json!({ "equals": "draft" }));
        assert_eq!(ops.len(), 1);
        assert!(scalar_eq(&ops[0], "draft"));
    }

    #[test]
    fn operators_not_equals() {
        let ops = ops_from(json!({ "not_equals": "draft" }));
        assert!(matches!(ops[0], FilterOp::NotEquals(ref v) if v == "draft"));
    }

    #[test]
    fn operators_contains_and_like_are_strings_only() {
        let ops = ops_from(json!({ "contains": "rust", "like": "%foo%" }));
        assert!(
            ops.iter()
                .any(|o| matches!(o, FilterOp::Contains(v) if v == "rust"))
        );
        assert!(
            ops.iter()
                .any(|o| matches!(o, FilterOp::Like(v) if v == "%foo%"))
        );
    }

    #[test]
    fn operators_comparison_with_integer() {
        let ops = ops_from(json!({ "greater_than": 10 }));
        assert!(matches!(ops[0], FilterOp::GreaterThan(ref v) if v == "10"));
    }

    #[test]
    fn operators_in_with_mixed_scalars() {
        let ops = ops_from(json!({ "in": ["a", 1, true] }));
        let FilterOp::In(vals) = &ops[0] else {
            panic!("expected In");
        };
        assert_eq!(
            vals,
            &["a".to_string(), "1".to_string(), "true".to_string()]
        );
    }

    #[test]
    fn operators_not_in_emits_filter_op() {
        let ops = ops_from(json!({ "not_in": ["x", "y"] }));
        assert!(matches!(&ops[0], FilterOp::NotIn(vs) if vs == &["x", "y"]));
    }

    #[test]
    fn operators_exists_and_not_exists_require_true() {
        let ops = ops_from(json!({ "exists": true }));
        assert!(matches!(ops[0], FilterOp::Exists));

        let ops = ops_from(json!({ "not_exists": true }));
        assert!(matches!(ops[0], FilterOp::NotExists));
    }

    /// Regression: `exists = false` used to be silently dropped (no op at
    /// all), widening the match set. It is now a hard error on both slots,
    /// matching the wire decoder.
    #[test]
    fn operators_exists_false_is_an_error_not_a_dropped_slot() {
        for key in ["exists", "not_exists"] {
            let ops: FilterOperators = serde_json::from_value(json!({ key: false })).unwrap();
            let err = ops.into_filter_ops().unwrap_err().to_string();
            assert!(err.contains("takes only `true`"), "{key}: {err}");
        }
    }

    #[test]
    fn operators_multiple_keys_each_produces_an_op() {
        let ops = ops_from(json!({
            "greater_than": 5,
            "less_than": 10,
        }));
        assert_eq!(ops.len(), 2);
        assert!(
            ops.iter()
                .any(|o| matches!(o, FilterOp::GreaterThan(v) if v == "5"))
        );
        assert!(
            ops.iter()
                .any(|o| matches!(o, FilterOp::LessThan(v) if v == "10"))
        );
    }

    // ── convert_where_clause: scalar + operators + or ───────────────

    #[test]
    fn where_clause_scalar_becomes_single_equals() {
        let mut w = HashMap::new();
        w.insert("status".to_string(), json!("published"));
        let clauses = convert_where_clause(w).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "status");
                assert!(scalar_eq(&f.op, "published"));
            }
            other => panic!("expected Single, got {other:?}"),
        }
    }

    #[test]
    fn where_clause_operator_becomes_single_with_op() {
        let mut w = HashMap::new();
        w.insert("title".to_string(), json!({ "contains": "lua" }));
        let clauses = convert_where_clause(w).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "title");
                assert!(matches!(&f.op, FilterOp::Contains(v) if v == "lua"));
            }
            other => panic!("expected Single, got {other:?}"),
        }
    }

    #[test]
    fn where_clause_or_key_becomes_or_clause() {
        let mut w = HashMap::new();
        w.insert(
            "or".to_string(),
            json!([
                { "author": "alice" },
                { "tag": "rust" },
            ]),
        );
        let clauses = convert_where_clause(w).unwrap();
        assert_eq!(clauses.len(), 1);
        let FilterClause::Or(alts) = &clauses[0] else {
            panic!("expected Or");
        };
        assert_eq!(alts.len(), 2);
        let (FilterClause::Single(f0), FilterClause::Single(f1)) = (&alts[0], &alts[1]) else {
            panic!("expected single-filter alternatives");
        };
        assert_eq!(f0.field, "author");
        assert_eq!(f1.field, "tag");
    }

    #[test]
    fn where_clause_or_value_not_array_errors() {
        let mut w = HashMap::new();
        w.insert("or".to_string(), json!({ "author": "alice" }));
        let err = convert_where_clause(w).unwrap_err();
        assert!(
            err.to_string().contains("'or' must be an array"),
            "got: {err}"
        );
    }
}
