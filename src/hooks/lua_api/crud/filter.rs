//! Shared filter-input primitives for the CRUD `*QueryInput` types
//! (which live next to their respective handlers in `crud/collection/`).
//!
//! The Lua user writes `where = { field = "value", other = { contains
//! = "x" }, ["or"] = { {...}, ... } }`. `mlua`'s `LuaSerdeExt` first
//! deserializes the input into the handler's `*QueryInput` struct,
//! which holds the raw `where` map as
//! `Option<HashMap<String, serde_json::Value>>`. Each handler then
//! calls [`convert_where_clause`], which hands the map to
//! [`decode_where_map`] — the ONE canonical `where` grammar shared by
//! every surface (gRPC, MCP, admin URL filters, and the Lua access
//! constraints).
//!
//! The `crap.FilterScalar` / `crap.FilterOperators` / `crap.FilterValue`
//! Lua annotations that document this grammar are rendered from the
//! canonical operator table (`db::query::FILTER_OP_SPECS`) by the typegen
//! static file — there is no Rust mirror struct to drift; the table itself
//! is pinned against `FilterOp` by a consistency test.

use std::collections::HashMap;

use mlua::{Error::RuntimeError, Result as LuaResult};

use crate::db::{FilterClause, query::filter::decode_where_map};

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
    use crate::db::FilterOp;
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
