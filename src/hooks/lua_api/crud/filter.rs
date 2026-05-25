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

use crate::db::{Filter, FilterClause, FilterOp};
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
    /// Field is not null (`IS NOT NULL`).
    #[lua(optional)]
    pub(crate) exists: Option<bool>,
    /// Field is null (`IS NULL`).
    #[lua(optional)]
    pub(crate) not_exists: Option<bool>,
}

impl FilterOperators {
    fn into_filter_ops(self) -> Vec<FilterOp> {
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
        if matches!(self.exists, Some(true)) {
            out.push(FilterOp::Exists);
        }
        if matches!(self.not_exists, Some(true)) {
            out.push(FilterOp::NotExists);
        }
        out
    }
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
    /// Build a `FilterValue` from a JSON value (the post-`from_value`
    /// shape the CRUD-input path produces). Used by
    /// `convert_where_clause` after Lua → JSON conversion.
    pub(crate) fn from_serde(value: serde_json::Value) -> Result<Self, String> {
        match value {
            serde_json::Value::Null => {
                Err("filter value must not be nil; use { exists = true/false } instead".into())
            }
            serde_json::Value::Bool(b) => Ok(Self::Scalar(FilterScalar::Bool(b))),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Self::Scalar(FilterScalar::Int(i)))
                } else if let Some(f) = n.as_f64() {
                    Ok(Self::Scalar(FilterScalar::Float(f)))
                } else {
                    Err(format!(
                        "filter number {n} cannot be represented as i64 or f64"
                    ))
                }
            }
            serde_json::Value::String(s) => Ok(Self::Scalar(FilterScalar::Str(s))),
            serde_json::Value::Object(map) => {
                let ops: FilterOperators = serde_json::from_value(serde_json::Value::Object(map))
                    .map_err(|e| e.to_string())?;
                Ok(Self::Operators(Box::new(ops)))
            }
            serde_json::Value::Array(_) => Err(
                "filter value cannot be an array; use { ['in'] = {...} } or an OR group instead"
                    .into(),
            ),
        }
    }

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
    pub(crate) fn into_filter_ops(self) -> Vec<FilterOp> {
        match self {
            Self::Scalar(s) => vec![FilterOp::Equals(s.to_filter_string())],
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

/// One AND-group inside a `where.or` array. Plain map of field →
/// filter value; serde resolves each value into the right
/// `FilterValue` variant.
type OrGroup = HashMap<String, serde_json::Value>;

/// Pull a `where` `HashMap` (post-`from_value` JSON) into `Vec<FilterClause>`.
/// The `"or"` key is treated specially — its value must be an array of
/// AND-groups, each of which is a `where`-shaped map. Called by every
/// `*QueryInput::into_find_query` to convert the typed `where_` field
/// into runtime `FilterClause`s.
pub(crate) fn convert_where_clause(
    where_: HashMap<String, serde_json::Value>,
) -> LuaResult<Vec<FilterClause>> {
    let mut out = Vec::new();

    for (field, value) in where_ {
        if field == "or" {
            let groups: Vec<OrGroup> = serde_json::from_value(value)
                .map_err(|e| RuntimeError(format!("invalid `or` clause: {e}")))?;
            out.push(FilterClause::Or(build_or_groups(groups)?));
            continue;
        }

        let fv = FilterValue::from_serde(value).map_err(RuntimeError)?;
        for op in fv.into_filter_ops() {
            out.push(FilterClause::Single(Filter {
                field: field.clone(),
                op,
            }));
        }
    }

    Ok(out)
}

fn build_or_groups(groups: Vec<OrGroup>) -> LuaResult<Vec<Vec<Filter>>> {
    let mut converted = Vec::with_capacity(groups.len());
    for group in groups {
        let mut filters = Vec::new();
        for (field, value) in group {
            let fv = FilterValue::from_serde(value).map_err(RuntimeError)?;
            for op in fv.into_filter_ops() {
                filters.push(Filter {
                    field: field.clone(),
                    op,
                });
            }
        }
        converted.push(filters);
    }
    Ok(converted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scalar_eq(op: &FilterOp, expected: &str) -> bool {
        matches!(op, FilterOp::Equals(v) if v == expected)
    }

    // ── FilterValue::from_serde dispatch ────────────────────────────

    #[test]
    fn filter_value_from_bool_becomes_scalar_bool() {
        let fv = FilterValue::from_serde(json!(true)).unwrap();
        assert!(matches!(fv, FilterValue::Scalar(FilterScalar::Bool(true))));
    }

    #[test]
    fn filter_value_from_integer_becomes_scalar_int() {
        let fv = FilterValue::from_serde(json!(42)).unwrap();
        assert!(matches!(fv, FilterValue::Scalar(FilterScalar::Int(42))));
    }

    #[test]
    fn filter_value_from_float_becomes_scalar_float() {
        let fv = FilterValue::from_serde(json!(3.5)).unwrap();
        let FilterValue::Scalar(FilterScalar::Float(f)) = fv else {
            panic!("expected Float scalar");
        };
        assert!((f - 3.5).abs() < f64::EPSILON);
    }

    #[test]
    fn filter_value_from_string_becomes_scalar_str() {
        let fv = FilterValue::from_serde(json!("published")).unwrap();
        assert!(matches!(fv, FilterValue::Scalar(FilterScalar::Str(ref s)) if s == "published"));
    }

    #[test]
    fn filter_value_from_object_becomes_operators() {
        let fv = FilterValue::from_serde(json!({ "contains": "rust" })).unwrap();
        let FilterValue::Operators(ops) = fv else {
            panic!("expected Operators variant");
        };
        assert_eq!(ops.contains.as_deref(), Some("rust"));
    }

    #[test]
    fn filter_value_from_null_errors() {
        let err = FilterValue::from_serde(json!(null)).unwrap_err();
        assert!(err.contains("must not be nil"), "got: {err}");
    }

    #[test]
    fn filter_value_from_array_errors() {
        let err = FilterValue::from_serde(json!(["a", "b"])).unwrap_err();
        assert!(err.contains("cannot be an array"), "got: {err}");
    }

    #[test]
    fn filter_value_unknown_operator_errors() {
        // `deny_unknown_fields` on FilterOperators surfaces unknown
        // operator keys as a serde error.
        let err = FilterValue::from_serde(json!({ "bad_op": "x" })).unwrap_err();
        assert!(err.contains("bad_op"), "got: {err}");
    }

    // ── FilterOperators → FilterOp expansion ────────────────────────

    fn ops_from(json_obj: serde_json::Value) -> Vec<FilterOp> {
        let ops: FilterOperators = serde_json::from_value(json_obj).unwrap();
        ops.into_filter_ops()
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

        let ops = ops_from(json!({ "exists": false }));
        assert!(ops.is_empty(), "exists=false should produce no op");

        let ops = ops_from(json!({ "not_exists": true }));
        assert!(matches!(ops[0], FilterOp::NotExists));
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
            FilterClause::Or(_) => panic!("expected Single, got Or"),
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
            FilterClause::Or(_) => panic!("expected Single, got Or"),
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
        let FilterClause::Or(groups) = &clauses[0] else {
            panic!("expected Or");
        };
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 1);
        assert_eq!(groups[0][0].field, "author");
        assert_eq!(groups[1][0].field, "tag");
    }

    #[test]
    fn where_clause_or_value_not_array_errors() {
        let mut w = HashMap::new();
        w.insert("or".to_string(), json!({ "author": "alice" }));
        let err = convert_where_clause(w).unwrap_err();
        assert!(
            err.to_string().contains("invalid `or` clause"),
            "got: {err}"
        );
    }
}
