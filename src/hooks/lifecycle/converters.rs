//! Pure Lua <-> Rust type conversion helpers (no DB access, no side effects).

use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

use crate::{
    core::{Document, FieldDefinition, FieldType},
    db::{
        Filter, FilterClause, FilterOp, FindQuery,
        query::{PaginationResult, cursor::CursorData, helpers::prefixed_name},
    },
    hooks::api,
};

// ── Lua <-> Rust type conversion helpers ────────────────────────────────────

/// Convert a Lua data table to HashMap<String, Value>.
/// Preserves nested tables (blocks, arrays, has-many IDs) unlike lua_table_to_hashmap
/// which only handles scalars.
pub(crate) fn lua_table_to_json_map(
    lua: &Lua,
    tbl: &Table,
) -> LuaResult<HashMap<String, JsonValue>> {
    let mut map = HashMap::new();

    for pair in tbl.pairs::<String, Value>() {
        let (k, v) = pair?;

        if matches!(v, Value::Nil) {
            continue;
        }

        map.insert(k, api::lua_to_json(lua, &v)?);
    }

    Ok(map)
}

/// Convert a Lua query table to a FindQuery.
/// Supports both simple filters (`{ status = "published" }`) and operator-based
/// filters (`{ title = { contains = "hello" } }`).
pub(crate) fn lua_table_to_find_query(tbl: &Table) -> LuaResult<(FindQuery, Option<i64>)> {
    let filters = parse_where_clause(tbl)?;

    let page: Option<i64> = tbl.get("page").ok();
    let offset: Option<i64> = if page.is_some() {
        None
    } else {
        tbl.get("offset").ok()
    };

    let select: Option<Vec<String>> = tbl.get::<Table>("select").ok().map(|t| {
        t.sequence_values::<String>()
            .filter_map(|r| r.ok())
            .collect()
    });

    let mut builder = FindQuery::builder().filters(filters);

    if let Ok(v) = tbl.get::<String>("order_by") {
        builder = builder.order_by(v);
    }
    if let Ok(v) = tbl.get::<i64>("limit") {
        builder = builder.limit(v);
    }
    if let Some(v) = offset {
        builder = builder.offset(v);
    }
    if let Some(v) = select {
        builder = builder.select(v);
    }
    if let Some(v) = parse_cursor(tbl, "after_cursor")? {
        builder = builder.after_cursor(v);
    }
    if let Some(v) = parse_cursor(tbl, "before_cursor")? {
        builder = builder.before_cursor(v);
    }
    if let Ok(v) = tbl.get::<String>("search") {
        builder = builder.search(v);
    }

    Ok((builder.build(), page))
}

/// Parse the `where` clause from a Lua query table into filter clauses.
fn parse_where_clause(tbl: &Table) -> LuaResult<Vec<FilterClause>> {
    let filters_tbl = match tbl.get::<Table>("where") {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()),
    };

    let mut clauses = Vec::new();

    for pair in filters_tbl.pairs::<String, Value>() {
        let (field, value) = pair?;

        if field == "or" {
            if let Value::Table(or_array) = value {
                clauses.push(FilterClause::Or(parse_or_groups(&or_array)?));
            }
            continue;
        }

        for f in parse_lua_filter_value(&field, &value)? {
            clauses.push(FilterClause::Single(f));
        }
    }

    Ok(clauses)
}

/// Parse an OR group array: `{ { status = "draft" }, { status = "review" } }`.
fn parse_or_groups(or_array: &Table) -> LuaResult<Vec<Vec<Filter>>> {
    let mut groups = Vec::new();

    for element in or_array.sequence_values::<Table>() {
        let tbl = element?;
        let mut group = Vec::new();

        for inner_pair in tbl.pairs::<String, Value>() {
            let (f, v) = inner_pair?;

            group.extend(parse_lua_filter_value(&f, &v)?);
        }

        groups.push(group);
    }

    Ok(groups)
}

/// Parse a single Lua filter value into one or more Filter structs.
/// Simple values produce one Equals filter; operator tables produce one per operator.
fn parse_lua_filter_value(field: &str, value: &Value) -> LuaResult<Vec<Filter>> {
    match value {
        Value::String(s) => Ok(vec![Filter {
            field: field.to_string(),
            op: FilterOp::Equals(s.to_str()?.to_string()),
        }]),
        Value::Integer(i) => Ok(vec![Filter {
            field: field.to_string(),
            op: FilterOp::Equals(i.to_string()),
        }]),
        Value::Number(n) => Ok(vec![Filter {
            field: field.to_string(),
            op: FilterOp::Equals(n.to_string()),
        }]),
        Value::Table(op_tbl) => {
            let mut filters = Vec::new();

            for op_pair in op_tbl.pairs::<String, Value>() {
                let (op_name, op_val) = op_pair?;
                let op = lua_parse_filter_op(&op_name, &op_val)?;

                filters.push(Filter {
                    field: field.to_string(),
                    op,
                });
            }
            Ok(filters)
        }
        _ => Ok(Vec::new()),
    }
}

/// Decode an optional cursor string from the query table.
fn parse_cursor(tbl: &Table, key: &str) -> LuaResult<Option<CursorData>> {
    match tbl.get::<Option<String>>(key).ok().flatten() {
        Some(s) => {
            Ok(Some(CursorData::decode(&s).map_err(|e| {
                RuntimeError(format!("Invalid cursor: {e:#}"))
            })?))
        }
        None => Ok(None),
    }
}

/// Parse a Lua filter operator name + value into a FilterOp.
pub(crate) fn lua_parse_filter_op(op_name: &str, value: &Value) -> LuaResult<FilterOp> {
    let to_string = |v: &Value| -> LuaResult<String> {
        match v {
            Value::String(s) => Ok(s.to_str()?.to_string()),
            Value::Integer(i) => Ok(i.to_string()),
            Value::Number(n) => Ok(n.to_string()),
            Value::Boolean(b) => Ok(b.to_string()),
            _ => Err(RuntimeError(
                "filter value must be string, number, or boolean".into(),
            )),
        }
    };

    match op_name {
        "equals" => Ok(FilterOp::Equals(to_string(value)?)),
        "not_equals" => Ok(FilterOp::NotEquals(to_string(value)?)),
        "like" => Ok(FilterOp::Like(to_string(value)?)),
        "contains" => Ok(FilterOp::Contains(to_string(value)?)),
        "greater_than" => Ok(FilterOp::GreaterThan(to_string(value)?)),
        "less_than" => Ok(FilterOp::LessThan(to_string(value)?)),
        "greater_than_or_equal" => Ok(FilterOp::GreaterThanOrEqual(to_string(value)?)),
        "less_than_or_equal" => Ok(FilterOp::LessThanOrEqual(to_string(value)?)),
        "in" => {
            let Value::Table(t) = value else {
                return Err(RuntimeError("'in' operator requires a table/array".into()));
            };

            let vals = collect_filter_values(t, &to_string)?;

            Ok(FilterOp::In(vals))
        }
        "not_in" => {
            let Value::Table(t) = value else {
                return Err(RuntimeError(
                    "'not_in' operator requires a table/array".into(),
                ));
            };

            let vals = collect_filter_values(t, &to_string)?;

            Ok(FilterOp::NotIn(vals))
        }
        "exists" => Ok(FilterOp::Exists),
        "not_exists" => Ok(FilterOp::NotExists),
        _ => Err(RuntimeError(format!(
            "unknown filter operator '{}'",
            op_name
        ))),
    }
}

/// Collect sequence values from a Lua table, converting each to a string.
fn collect_filter_values(
    tbl: &Table,
    to_string: &impl Fn(&Value) -> LuaResult<String>,
) -> LuaResult<Vec<String>> {
    let mut vals = Vec::new();
    for v in tbl.clone().sequence_values::<Value>() {
        vals.push(to_string(&v?)?);
    }
    Ok(vals)
}

/// Convert a Lua data table to a HashMap<String, String> for create/update.
pub(crate) fn lua_table_to_hashmap(tbl: &Table) -> LuaResult<HashMap<String, String>> {
    let mut map = HashMap::new();

    for pair in tbl.pairs::<String, Value>() {
        let (k, v) = pair?;
        let s = match v {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Boolean(b) => b.to_string(),
            Value::Nil => continue,
            _ => continue,
        };

        map.insert(k, s);
    }

    Ok(map)
}

/// Flatten group fields from a Lua data table into the data map.
/// Converts `seo = { meta_title = "X" }` → `seo__meta_title = "X"`.
pub(crate) fn flatten_lua_groups(
    tbl: &Table,
    fields: &[FieldDefinition],
    data: &mut HashMap<String, String>,
) -> LuaResult<()> {
    for field in fields {
        if field.field_type != FieldType::Group {
            continue;
        }

        let Ok(sub_table) = tbl.get::<Table>(field.name.as_str()) else {
            continue;
        };

        for sub in &field.fields {
            let Ok(val) = sub_table.get::<Value>(sub.name.as_str()) else {
                continue;
            };

            let s = match val {
                Value::String(s) => s.to_str()?.to_string(),
                Value::Integer(i) => i.to_string(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };

            data.insert(prefixed_name(&field.name, &sub.name), s);
        }
    }

    Ok(())
}

/// Convert a [`PaginationResult`] into an mlua table.
pub(crate) fn pagination_result_to_lua_table(lua: &Lua, pr: &PaginationResult) -> LuaResult<Table> {
    let t = lua.create_table()?;
    t.set("totalDocs", pr.total_docs)?;
    t.set("limit", pr.limit)?;
    t.set("hasNextPage", pr.has_next_page)?;
    t.set("hasPrevPage", pr.has_prev_page)?;

    if let Some(v) = pr.total_pages {
        t.set("totalPages", v)?;
    }
    if let Some(v) = pr.page {
        t.set("page", v)?;
    }
    if let Some(v) = pr.page_start {
        t.set("pageStart", v)?;
    }
    if let Some(v) = pr.prev_page {
        t.set("prevPage", v)?;
    }
    if let Some(v) = pr.next_page {
        t.set("nextPage", v)?;
    }
    if let Some(ref v) = pr.start_cursor {
        t.set("startCursor", v.clone())?;
    }
    if let Some(ref v) = pr.end_cursor {
        t.set("endCursor", v.clone())?;
    }

    Ok(t)
}

/// Convert a Document to a Lua table.
pub(crate) fn document_to_lua_table(lua: &Lua, doc: &Document) -> LuaResult<Table> {
    let tbl = lua.create_table()?;

    tbl.set("id", &*doc.id)?;

    for (k, v) in &doc.fields {
        tbl.set(k.as_str(), api::json_to_lua(lua, v)?)?;
    }

    if let Some(ref ts) = doc.created_at {
        tbl.set("created_at", ts.as_str())?;
    }

    if let Some(ref ts) = doc.updated_at {
        tbl.set("updated_at", ts.as_str())?;
    }

    Ok(tbl)
}

/// Convert a find result (documents + total) to a Lua table.
pub(crate) fn find_result_to_lua(
    lua: &Lua,
    docs: &[Document],
    pagination: Table,
) -> LuaResult<Table> {
    let tbl = lua.create_table()?;
    let docs_tbl = lua.create_table()?;

    for (i, doc) in docs.iter().enumerate() {
        docs_tbl.set(i + 1, document_to_lua_table(lua, doc)?)?;
    }

    tbl.set("documents", docs_tbl)?;
    tbl.set("pagination", pagination)?;

    Ok(tbl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::DocumentBuilder;
    use crate::core::field::{FieldDefinition, FieldType};
    use mlua::Lua;
    use serde_json::json;

    // --- lua_parse_filter_op tests ---

    #[test]
    fn test_filter_op_equals() {
        let lua = Lua::new();
        let s = lua.create_string("hello").unwrap();
        let op = lua_parse_filter_op("equals", &Value::String(s)).unwrap();
        assert!(matches!(op, FilterOp::Equals(ref v) if v == "hello"));
    }

    #[test]
    fn test_filter_op_not_equals() {
        let lua = Lua::new();
        let s = lua.create_string("world").unwrap();
        let op = lua_parse_filter_op("not_equals", &Value::String(s)).unwrap();
        assert!(matches!(op, FilterOp::NotEquals(ref v) if v == "world"));
    }

    #[test]
    fn test_filter_op_contains() {
        let lua = Lua::new();
        let s = lua.create_string("search").unwrap();
        let op = lua_parse_filter_op("contains", &Value::String(s)).unwrap();
        assert!(matches!(op, FilterOp::Contains(ref v) if v == "search"));
    }

    #[test]
    fn test_filter_op_like() {
        let lua = Lua::new();
        let s = lua.create_string("%pattern%").unwrap();
        let op = lua_parse_filter_op("like", &Value::String(s)).unwrap();
        assert!(matches!(op, FilterOp::Like(ref v) if v == "%pattern%"));
    }

    #[test]
    fn test_filter_op_greater_than() {
        let op = lua_parse_filter_op("greater_than", &Value::Integer(10)).unwrap();
        assert!(matches!(op, FilterOp::GreaterThan(ref v) if v == "10"));
    }

    #[test]
    fn test_filter_op_less_than() {
        let op = lua_parse_filter_op("less_than", &Value::Integer(5)).unwrap();
        assert!(matches!(op, FilterOp::LessThan(ref v) if v == "5"));
    }

    #[test]
    fn test_filter_op_greater_than_or_equal() {
        let op = lua_parse_filter_op("greater_than_or_equal", &Value::Number(3.15)).unwrap();
        assert!(matches!(op, FilterOp::GreaterThanOrEqual(ref v) if v == "3.15"));
    }

    #[test]
    fn test_filter_op_less_than_or_equal() {
        let op = lua_parse_filter_op("less_than_or_equal", &Value::Boolean(true)).unwrap();
        assert!(matches!(op, FilterOp::LessThanOrEqual(ref v) if v == "true"));
    }

    #[test]
    fn test_filter_op_in() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set(1, "a").unwrap();
        tbl.set(2, "b").unwrap();
        tbl.set(3, "c").unwrap();
        let op = lua_parse_filter_op("in", &Value::Table(tbl)).unwrap();
        match op {
            FilterOp::In(vals) => assert_eq!(vals, vec!["a", "b", "c"]),
            _ => panic!("Expected In"),
        }
    }

    #[test]
    fn test_filter_op_not_in() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set(1, "x").unwrap();
        tbl.set(2, "y").unwrap();
        let op = lua_parse_filter_op("not_in", &Value::Table(tbl)).unwrap();
        match op {
            FilterOp::NotIn(vals) => assert_eq!(vals, vec!["x", "y"]),
            _ => panic!("Expected NotIn"),
        }
    }

    #[test]
    fn test_filter_op_in_requires_table() {
        let result = lua_parse_filter_op("in", &Value::Integer(42));
        assert!(result.is_err());
    }

    #[test]
    fn test_filter_op_not_in_requires_table() {
        let result = lua_parse_filter_op("not_in", &Value::Integer(42));
        assert!(result.is_err());
    }

    #[test]
    fn test_filter_op_exists() {
        let op = lua_parse_filter_op("exists", &Value::Nil).unwrap();
        assert!(matches!(op, FilterOp::Exists));
    }

    #[test]
    fn test_filter_op_not_exists() {
        let op = lua_parse_filter_op("not_exists", &Value::Nil).unwrap();
        assert!(matches!(op, FilterOp::NotExists));
    }

    #[test]
    fn test_filter_op_unknown() {
        let result = lua_parse_filter_op("bogus", &Value::Nil);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unknown filter operator")
        );
    }

    // --- lua_table_to_hashmap tests ---

    #[test]
    fn test_hashmap_from_strings() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("title", "Hello").unwrap();
        tbl.set("slug", "hello").unwrap();
        let map = lua_table_to_hashmap(&tbl).unwrap();
        assert_eq!(map.get("title").unwrap(), "Hello");
        assert_eq!(map.get("slug").unwrap(), "hello");
    }

    #[test]
    fn test_hashmap_from_mixed_types() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("title", "Hello").unwrap();
        tbl.set("count", 42).unwrap();
        tbl.set("ratio", 3.15).unwrap();
        tbl.set("active", true).unwrap();
        let map = lua_table_to_hashmap(&tbl).unwrap();
        assert_eq!(map.get("title").unwrap(), "Hello");
        assert_eq!(map.get("count").unwrap(), "42");
        assert_eq!(map.get("ratio").unwrap(), "3.15");
        assert_eq!(map.get("active").unwrap(), "true");
    }

    #[test]
    fn test_hashmap_skips_nil() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("title", "Hello").unwrap();
        // Nil values are skipped
        let map = lua_table_to_hashmap(&tbl).unwrap();
        assert_eq!(map.len(), 1);
    }

    // --- flatten_lua_groups tests ---

    #[test]
    fn test_flatten_groups_basic() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let seo = lua.create_table().unwrap();
        seo.set("meta_title", "My Title").unwrap();
        seo.set("meta_description", "My Desc").unwrap();
        tbl.set("seo", seo).unwrap();
        tbl.set("title", "Hello").unwrap();

        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("meta_title", FieldType::Text).build(),
                    FieldDefinition::builder("meta_description", FieldType::Textarea).build(),
                ])
                .build(),
            FieldDefinition::builder("title", FieldType::Text).build(),
        ];

        let mut data = HashMap::new();
        flatten_lua_groups(&tbl, &fields, &mut data).unwrap();
        assert_eq!(data.get("seo__meta_title").unwrap(), "My Title");
        assert_eq!(data.get("seo__meta_description").unwrap(), "My Desc");
        // Non-group fields are not touched
        assert!(!data.contains_key("title"));
    }

    #[test]
    fn test_flatten_groups_missing_subtable() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        // No "seo" key at all

        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("meta_title", FieldType::Text).build(),
                ])
                .build(),
        ];

        let mut data = HashMap::new();
        flatten_lua_groups(&tbl, &fields, &mut data).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn test_flatten_groups_numeric_values() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let metrics = lua.create_table().unwrap();
        metrics.set("views", 100).unwrap();
        metrics.set("rating", 4.5).unwrap();
        tbl.set("metrics", metrics).unwrap();

        let fields = vec![
            FieldDefinition::builder("metrics", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("views", FieldType::Number).build(),
                    FieldDefinition::builder("rating", FieldType::Number).build(),
                ])
                .build(),
        ];

        let mut data = HashMap::new();
        flatten_lua_groups(&tbl, &fields, &mut data).unwrap();
        assert_eq!(data.get("metrics__views").unwrap(), "100");
        assert_eq!(data.get("metrics__rating").unwrap(), "4.5");
    }

    // --- document_to_lua_table tests ---

    #[test]
    fn test_document_to_lua_basic() {
        let lua = Lua::new();
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));
        fields.insert("count".to_string(), json!(42));

        let doc = DocumentBuilder::new("abc123")
            .fields(fields)
            .created_at(Some("2024-01-01T00:00:00Z"))
            .updated_at(Some("2024-01-02T00:00:00Z"))
            .build();

        let tbl = document_to_lua_table(&lua, &doc).unwrap();
        let id: String = tbl.get("id").unwrap();
        let title: String = tbl.get("title").unwrap();
        let count: i64 = tbl.get("count").unwrap();
        let created: String = tbl.get("created_at").unwrap();
        let updated: String = tbl.get("updated_at").unwrap();
        assert_eq!(id, "abc123");
        assert_eq!(title, "Hello");
        assert_eq!(count, 42);
        assert_eq!(created, "2024-01-01T00:00:00Z");
        assert_eq!(updated, "2024-01-02T00:00:00Z");
    }

    #[test]
    fn test_document_to_lua_no_timestamps() {
        let lua = Lua::new();
        let doc = DocumentBuilder::new("xyz").build();

        let tbl = document_to_lua_table(&lua, &doc).unwrap();
        let id: String = tbl.get("id").unwrap();
        assert_eq!(id, "xyz");
        // No timestamps set
        let created: Value = tbl.get("created_at").unwrap();
        assert!(matches!(created, Value::Nil));
    }

    // --- find_result_to_lua tests ---

    #[test]
    fn test_find_result_to_lua_basic() {
        let lua = Lua::new();
        let docs = vec![
            DocumentBuilder::new("a").build(),
            DocumentBuilder::new("b").build(),
        ];

        let pg = lua.create_table().unwrap();
        pg.set("total", 10i64).unwrap();
        pg.set("limit", 5i64).unwrap();
        pg.set("has_next", true).unwrap();
        pg.set("has_prev", false).unwrap();
        let tbl = find_result_to_lua(&lua, &docs, pg).unwrap();
        let pagination: mlua::Table = tbl.get("pagination").unwrap();
        let total: i64 = pagination.get("total").unwrap();
        assert_eq!(total, 10);
        let docs_tbl: mlua::Table = tbl.get("documents").unwrap();
        assert_eq!(docs_tbl.raw_len(), 2);
        let first: mlua::Table = docs_tbl.get(1).unwrap();
        let id: String = first.get("id").unwrap();
        assert_eq!(id, "a");
    }

    #[test]
    fn test_find_result_to_lua_empty() {
        let lua = Lua::new();
        let pg = lua.create_table().unwrap();
        pg.set("total", 0i64).unwrap();
        pg.set("limit", 10i64).unwrap();
        pg.set("has_next", false).unwrap();
        pg.set("has_prev", false).unwrap();
        let tbl = find_result_to_lua(&lua, &[], pg).unwrap();
        let pagination: mlua::Table = tbl.get("pagination").unwrap();
        let total: i64 = pagination.get("total").unwrap();
        assert_eq!(total, 0);
        let docs_tbl: mlua::Table = tbl.get("documents").unwrap();
        assert_eq!(docs_tbl.raw_len(), 0);
    }

    // --- lua_table_to_find_query tests ---

    #[test]
    fn test_find_query_empty() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let (query, page) = lua_table_to_find_query(&tbl).unwrap();
        assert!(query.filters.is_empty());
        assert!(query.order_by.is_none());
        assert!(query.limit.is_none());
        assert!(query.offset.is_none());
        assert!(query.select.is_none());
        assert!(page.is_none());
    }

    #[test]
    fn test_find_query_with_pagination() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("limit", 10i64).unwrap();
        tbl.set("offset", 20i64).unwrap();
        tbl.set("order_by", "-created_at").unwrap();
        let (query, page) = lua_table_to_find_query(&tbl).unwrap();
        assert_eq!(query.limit, Some(10));
        assert_eq!(query.offset, Some(20));
        assert_eq!(query.order_by.as_deref(), Some("-created_at"));
        assert!(page.is_none());
    }

    #[test]
    fn test_find_query_with_page() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("limit", 10i64).unwrap();
        tbl.set("page", 3i64).unwrap();
        let (query, page) = lua_table_to_find_query(&tbl).unwrap();
        assert_eq!(query.limit, Some(10));
        assert!(query.offset.is_none()); // page overrides offset
        assert_eq!(page, Some(3));
    }

    #[test]
    fn test_find_query_page_overrides_offset() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("page", 2i64).unwrap();
        tbl.set("offset", 99i64).unwrap();
        let (query, page) = lua_table_to_find_query(&tbl).unwrap();
        assert!(query.offset.is_none()); // page takes precedence
        assert_eq!(page, Some(2));
    }

    #[test]
    fn test_find_query_with_select() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let select = lua.create_table().unwrap();
        select.set(1, "title").unwrap();
        select.set(2, "slug").unwrap();
        tbl.set("select", select).unwrap();
        let (query, _) = lua_table_to_find_query(&tbl).unwrap();
        assert_eq!(query.select.as_ref().unwrap(), &["title", "slug"]);
    }

    #[test]
    fn test_find_query_with_simple_filter() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let filters = lua.create_table().unwrap();
        filters.set("status", "published").unwrap();
        tbl.set("where", filters).unwrap();
        let (query, _) = lua_table_to_find_query(&tbl).unwrap();
        assert_eq!(query.filters.len(), 1);
        match &query.filters[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "status");
                assert!(matches!(&f.op, FilterOp::Equals(v) if v == "published"));
            }
            _ => panic!("Expected Single filter"),
        }
    }

    #[test]
    fn test_find_query_with_operator_filter() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let filters = lua.create_table().unwrap();
        let op = lua.create_table().unwrap();
        op.set("contains", "hello").unwrap();
        filters.set("title", op).unwrap();
        tbl.set("where", filters).unwrap();
        let (query, _) = lua_table_to_find_query(&tbl).unwrap();
        assert_eq!(query.filters.len(), 1);
        match &query.filters[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "title");
                assert!(matches!(&f.op, FilterOp::Contains(v) if v == "hello"));
            }
            _ => panic!("Expected Single filter"),
        }
    }

    // --- lua_table_to_json_map tests ---

    #[test]
    fn test_json_map_basic() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("title", "Hello").unwrap();
        tbl.set("count", 42).unwrap();
        tbl.set("active", true).unwrap();
        let map = lua_table_to_json_map(&lua, &tbl).unwrap();
        assert_eq!(map.get("title").unwrap(), &json!("Hello"));
        assert_eq!(map.get("count").unwrap(), &json!(42));
        assert_eq!(map.get("active").unwrap(), &json!(true));
    }

    #[test]
    fn test_json_map_skips_nil() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("title", "Hello").unwrap();
        // Setting a key to nil removes it from Lua table iteration
        let map = lua_table_to_json_map(&lua, &tbl).unwrap();
        assert_eq!(map.len(), 1);
    }
}
