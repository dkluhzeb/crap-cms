//! Filters on list-valued fields — a scalar has-many list (a JSON array in a
//! column or inside a row), the id list of a has-many relationship or upload
//! stored inside a row, and a has-many relationship or upload's junction (one
//! row per id).
//!
//! All read a filter the same way, element by element:
//! - `equals`, `like`, `contains`, the ordered comparisons, `in` and `exists`
//!   match when **some** element matches;
//! - `not_equals`, `not_in` and `not_exists` match when **no** element matches
//!   the positive operator (`equals`, `in`, `exists`).
//!
//! An empty list and a NULL value hold no elements: every positive operator
//! misses them and every negative one matches them. Every stored list is NULL
//! or a JSON array — the schema sync keeps it so (see
//! `db::migrate::has_many_lists`) — so the expansion never meets anything else.
//! The in-memory evaluator applies the same reading.

use anyhow::Result;

use super::operators::build_op_condition;
use crate::{
    core::{FieldDefinition, FieldType},
    db::{
        DbConnection, DbValue, Filter, FilterOp,
        query::{helpers::is_polymorphic, poly_ref},
    },
};

/// The alias a stored list's element expansion takes. Distinct from the `j{n}`
/// aliases of nested row expansions it may sit inside.
const ELEMENT_ALIAS: &str = "crap_el";

/// A leaf holding a list, read element by element.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::db::query::filter) enum ListLeaf {
    /// A scalar has-many list: each element compares as the field's single
    /// value would.
    Scalar(FieldType),
    /// The id list of a has-many relationship or upload stored inside a row.
    /// Each element compares as the id a junction's `related_id` holds — the id
    /// half of a polymorphic `collection/id` entry.
    References { polymorphic: bool },
}

impl ListLeaf {
    /// How `field` is read as a list, or `None` when it holds a single value.
    /// A top-level has-many reference has no stored list — its junction rows
    /// are its elements.
    pub(in crate::db::query::filter) fn of(field: &FieldDefinition) -> Option<Self> {
        if field.is_has_many_scalar() {
            return Some(Self::Scalar(field.field_type.clone()));
        }

        if field.is_has_many_reference() {
            let polymorphic = is_polymorphic(field);

            return Some(Self::References { polymorphic });
        }

        None
    }

    /// The type each element compares as.
    pub(in crate::db::query::filter) fn element_type(&self) -> FieldType {
        match self {
            Self::Scalar(field_type) => field_type.clone(),
            Self::References { .. } => FieldType::Text,
        }
    }
}

/// A list-valued leaf and the SQL expression its stored list is read from.
pub(super) struct ListExpr<'a> {
    expr: &'a str,
    leaf: &'a ListLeaf,
}

impl<'a> ListExpr<'a> {
    /// `expr` must be qualified (`"posts"."tags"`) or an expression over
    /// qualified columns: the expansion's own columns (`value`, `type`, `key`,
    /// … on `SQLite`) would otherwise shadow a bare column of the same name.
    pub(super) fn new(expr: &'a str, leaf: &'a ListLeaf) -> Self {
        Self { expr, leaf }
    }
}

/// Whether a list filter asks for some element to match, or for none to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Quantifier {
    /// Some element matches.
    Any,
    /// No element matches.
    NoElement,
}

/// How `op` quantifies over a list's elements, and the operator each element
/// is tested with: a negative operator asks that no element match its
/// positive counterpart, every other that some element match it.
pub(super) fn quantify(op: &FilterOp) -> (Quantifier, FilterOp) {
    match op {
        FilterOp::NotEquals(v) => (Quantifier::NoElement, FilterOp::Equals(v.clone())),
        FilterOp::NotIn(vs) => (Quantifier::NoElement, FilterOp::In(vs.clone())),
        FilterOp::NotExists => (Quantifier::NoElement, FilterOp::Exists),
        other => (Quantifier::Any, other.clone()),
    }
}

/// Wrap the element query `subquery` builds for the per-element operator in
/// `EXISTS` or `NOT EXISTS`, as `op` quantifies.
pub(super) fn build_quantified(
    op: &FilterOp,
    subquery: impl FnOnce(&FilterOp) -> Result<String>,
) -> Result<String> {
    let (quantifier, element_op) = quantify(op);
    let select = subquery(&element_op)?;

    Ok(match quantifier {
        Quantifier::Any => format!("EXISTS ({select})"),
        Quantifier::NoElement => format!("NOT EXISTS ({select})"),
    })
}

/// The SQL condition applying the filter `f` to the elements of `list`. Each
/// element compares as the leaf's element would ([`ListLeaf::element_type`]) —
/// a Number list numerically, a Text list and a reference list's ids in their
/// stored canonical form.
///
/// # Errors
///
/// Returns a validation error when an operand does not fit the element type.
pub(super) fn build_list_condition(
    conn: &dyn DbConnection,
    f: &Filter,
    list: &ListExpr<'_>,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    let source = conn.json_each_source(list.expr, ELEMENT_ALIAS);
    let element = element_expr(conn, list.leaf);
    let element_type = list.leaf.element_type();

    build_quantified(&f.op, |element_op| {
        let condition = build_op_condition(
            conn,
            &f.field,
            &element,
            element_op,
            Some(&element_type),
            params,
        )?;

        Ok(format!("SELECT 1 FROM {source} WHERE {condition}"))
    })
}

/// One element of the expanded list. A Number element is text on Postgres
/// (`jsonb_array_elements_text`), so it is cast to compare numerically; a
/// polymorphic reference compares by the id after its collection.
fn element_expr(conn: &dyn DbConnection, leaf: &ListLeaf) -> String {
    let value = format!("{ELEMENT_ALIAS}.value");

    match leaf {
        ListLeaf::Scalar(FieldType::Number) => conn.json_number_cast(&value),
        ListLeaf::References { polymorphic: true } => conn.text_after(&value, poly_ref::SEPARATOR),
        _ => value,
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::collections::HashMap;

    use serde_json::{Value, from_str, json};

    use super::*;
    use crate::{
        core::{BlockDefinition, DocumentFields, RelationshipConfig},
        db::{
            FilterClause, InMemoryConn,
            query::{
                filter::{build_where_clause, memory::matches_constraints_typed},
                helpers::{ListPlace, parse_has_many_scalar},
            },
        },
    };

    fn sql(op: FilterOp, leaf: &ListLeaf) -> (String, Vec<DbValue>) {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let filter = Filter {
            field: "tags".to_string(),
            op,
        };
        let list = ListExpr::new("\"posts\".\"tags\"", leaf);
        let sql = build_list_condition(&conn, &filter, &list, &mut params).unwrap();

        (sql, params)
    }

    const EXPANDED: &str = "json_each(\"posts\".\"tags\") AS crap_el";

    #[test]
    fn negative_operators_ask_that_no_element_match_the_positive_one() {
        let cases = [
            (FilterOp::NotEquals("a".into()), "equals"),
            (FilterOp::NotIn(vec!["a".into()]), "in"),
            (FilterOp::NotExists, "exists"),
        ];

        for (op, positive) in cases {
            let (quantifier, element_op) = quantify(&op);
            assert_eq!(quantifier, Quantifier::NoElement, "{op:?}");
            assert_eq!(element_op.op_name(), positive);
        }

        for op in [
            FilterOp::Equals("a".into()),
            FilterOp::Like("a%".into()),
            FilterOp::Contains("a".into()),
            FilterOp::GreaterThan("1".into()),
            FilterOp::In(vec!["a".into()]),
            FilterOp::Exists,
        ] {
            let (quantifier, element_op) = quantify(&op);
            assert_eq!(quantifier, Quantifier::Any, "{op:?}");
            assert_eq!(element_op.op_name(), op.op_name());
        }
    }

    /// The stored list is expanded as it is: the schema sync keeps every
    /// stored list NULL or a JSON array, so no guard wraps it.
    #[test]
    fn equals_tests_some_element_of_the_list() {
        let (sql, params) = sql(
            FilterOp::Equals("a".into()),
            &ListLeaf::Scalar(FieldType::Text),
        );

        assert_eq!(
            sql,
            format!("EXISTS (SELECT 1 FROM {EXPANDED} WHERE crap_el.value = ?1)")
        );
        assert_eq!(params, vec![DbValue::Text("a".into())]);
    }

    #[test]
    fn not_in_asks_that_no_element_be_in_the_set() {
        let (sql, params) = sql(
            FilterOp::NotIn(vec!["1".into(), "2.5".into()]),
            &ListLeaf::Scalar(FieldType::Number),
        );

        assert_eq!(
            sql,
            format!("NOT EXISTS (SELECT 1 FROM {EXPANDED} WHERE crap_el.value IN (?1, ?2))")
        );
        assert_eq!(params, vec![DbValue::Real(1.0), DbValue::Real(2.5)]);
    }

    #[test]
    fn not_exists_asks_for_no_element_at_all() {
        let (sql, _) = sql(FilterOp::NotExists, &ListLeaf::Scalar(FieldType::Select));

        assert_eq!(
            sql,
            format!("NOT EXISTS (SELECT 1 FROM {EXPANDED} WHERE crap_el.value IS NOT NULL)")
        );
    }

    /// A polymorphic reference list compares the id after each entry's
    /// collection, as its junction's `related_id` holds it.
    #[test]
    fn a_polymorphic_reference_element_compares_its_id() {
        let leaf = ListLeaf::References { polymorphic: true };
        let (sql, _) = sql(FilterOp::Equals("a".into()), &leaf);

        assert_eq!(
            sql,
            format!(
                "EXISTS (SELECT 1 FROM {EXPANDED} WHERE \
                 substr(crap_el.value, instr(crap_el.value, '/') + 1) = ?1)"
            )
        );
    }

    /// The text after a separator is the whole text when it holds none, and
    /// only the first separator splits.
    #[test]
    fn text_after_splits_at_the_first_separator() {
        let conn = InMemoryConn::open();
        let sql = format!("SELECT {} AS rest", conn.text_after("?1", "/"));

        let rest = |text: &str| {
            conn.query_one(&sql, &[DbValue::Text(text.into())])
                .unwrap()
                .unwrap()
                .get_string("rest")
                .unwrap()
        };

        assert_eq!(rest("posts/a"), "a");
        assert_eq!(rest("a"), "a");
        assert_eq!(rest("posts/a/b"), "a/b");
    }

    // ── Executed list semantics, SQL and in-memory ────────────────────────

    /// The stored lists: `(id, tags, scores)`. `tags` is a Text list, `scores`
    /// a Number list. The same ids the `tags` lists hold are the `refs`
    /// junction's, an `items` row's `related` list and a `content` block's
    /// polymorphic `links` list, so every kind of list compares alike.
    const ROWS: [(&str, Option<&str>, Option<&str>); 4] = [
        ("a", Some(r#"["a","b"]"#), Some("[1,2.5]")),
        ("b", Some(r#"["c"]"#), Some("[10]")),
        ("empty", Some("[]"), Some("[]")),
        ("null", None, None),
    ];

    fn references(name: &str, collections: &[&str]) -> FieldDefinition {
        let mut config = RelationshipConfig::new(collections[0], true);

        if collections.len() > 1 {
            config.polymorphic = collections.iter().map(|c| (*c).into()).collect();
        }

        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(config)
            .build()
    }

    fn list_fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .build(),
            FieldDefinition::builder("scores", FieldType::Number)
                .has_many(true)
                .build(),
            references("refs", &["things"]),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![references("related", &["things"])])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "link",
                    vec![references("links", &["things", "others"])],
                )])
                .build(),
        ]
    }

    /// The ids a row's `tags` list holds, as JSON strings.
    fn tag_ids(tags: Option<&str>) -> Option<Vec<Value>> {
        tags.map(|raw| from_str::<Vec<Value>>(raw).unwrap())
    }

    /// A `content` block's data: the `tags` ids as polymorphic `things/…`
    /// entries, the key missing when the list is unset.
    fn block_data(tags: Option<&str>) -> Value {
        let Some(ids) = tag_ids(tags) else {
            return json!({ "_block_type": "link" });
        };
        let links: Vec<Value> = ids
            .iter()
            .map(|id| json!(format!("things/{}", id.as_str().unwrap())))
            .collect();

        json!({ "_block_type": "link", "links": links })
    }

    /// A `posts` table holding [`ROWS`], and the `refs` junction, `items` rows
    /// and `content` blocks holding the same ids — one row and one block per
    /// document.
    fn list_db() -> InMemoryConn {
        let conn = InMemoryConn::open();
        conn.setup(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, tags TEXT, scores TEXT);
             CREATE TABLE posts_refs (parent_id TEXT, related_id TEXT);
             CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, related TEXT);
             CREATE TABLE posts_content (id TEXT PRIMARY KEY, parent_id TEXT,
               _block_type TEXT, data TEXT);
             INSERT INTO posts_refs VALUES ('a', 'a'), ('a', 'b'), ('b', 'c');",
        );

        let text = |v: Option<&str>| v.map_or(DbValue::Null, |v| DbValue::Text(v.into()));

        for (id, tags, scores) in ROWS {
            let id_value = DbValue::Text(id.into());
            let row_id = DbValue::Text(format!("row-{id}"));
            let data = DbValue::Text(block_data(tags).to_string());

            conn.execute(
                "INSERT INTO posts (id, tags, scores) VALUES (?1, ?2, ?3)",
                &[id_value.clone(), text(tags), text(scores)],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO posts_items (id, parent_id, related) VALUES (?1, ?2, ?3)",
                &[row_id.clone(), id_value.clone(), text(tags)],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO posts_content VALUES (?1, ?2, 'link', ?3)",
                &[row_id, id_value, data],
            )
            .unwrap();
        }

        conn
    }

    fn single(field: &str, op: FilterOp) -> Vec<FilterClause> {
        vec![FilterClause::Single(Filter {
            field: field.to_string(),
            op,
        })]
    }

    /// The ids SQL matches for `field op`, sorted.
    fn sql_ids(conn: &InMemoryConn, field: &str, op: &FilterOp) -> Vec<String> {
        let mut params = Vec::new();
        let filters = single(field, op.clone());
        let clause =
            build_where_clause(conn, &filters, "posts", &list_fields(), None, &mut params).unwrap();

        conn.query_all(
            &format!("SELECT id FROM posts{clause} ORDER BY id"),
            &params,
        )
        .unwrap()
        .iter()
        .map(|row| row.get_string("id").unwrap())
        .collect()
    }

    /// A document of [`ROWS`] as a read returns it: lists decoded, a NULL
    /// column null, the `refs` ids the junction holds for it, and its `items`
    /// row and `content` block.
    fn document(tags: Option<&str>, scores: Option<&str>) -> DocumentFields {
        let decode = |ft: &FieldType, raw: Option<&str>| {
            raw.map_or(Value::Null, |raw| {
                parse_has_many_scalar(ft, &Value::String(raw.into()), ListPlace::Column)
            })
        };
        let related = tag_ids(tags).map_or(Value::Null, Value::Array);
        let refs = Value::Array(tag_ids(tags).unwrap_or_default());

        DocumentFields::from(HashMap::from([
            ("tags".to_string(), decode(&FieldType::Text, tags)),
            ("scores".to_string(), decode(&FieldType::Number, scores)),
            ("refs".to_string(), refs),
            ("items".to_string(), json!([{ "related": related }])),
            ("content".to_string(), json!([block_data(tags)])),
        ]))
    }

    /// The ids the in-memory evaluator matches for `field op`.
    fn memory_ids(field: &str, op: &FilterOp) -> Vec<String> {
        let fields = list_fields();
        let filters = single(field, op.clone());

        ROWS.iter()
            .filter(|(_, tags, scores)| {
                matches_constraints_typed(&document(*tags, *scores), &filters, &fields)
            })
            .map(|(id, _, _)| (*id).to_string())
            .collect()
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|id| (*id).to_string()).collect()
    }

    fn text_cases() -> Vec<(FilterOp, Vec<String>)> {
        vec![
            (FilterOp::Equals("a".into()), ids(&["a"])),
            (FilterOp::Equals("z".into()), ids(&[])),
            (
                FilterOp::NotEquals("a".into()),
                ids(&["b", "empty", "null"]),
            ),
            (FilterOp::In(vec!["a".into(), "c".into()]), ids(&["a", "b"])),
            (
                FilterOp::NotIn(vec!["a".into(), "c".into()]),
                ids(&["empty", "null"]),
            ),
            (FilterOp::Like("B".into()), ids(&["a"])),
            (FilterOp::Contains("c".into()), ids(&["b"])),
            (FilterOp::Exists, ids(&["a", "b"])),
            (FilterOp::NotExists, ids(&["empty", "null"])),
        ]
    }

    fn number_cases() -> Vec<(FilterOp, Vec<String>)> {
        vec![
            (FilterOp::Equals("2.5".into()), ids(&["a"])),
            (FilterOp::Equals("10".into()), ids(&["b"])),
            (
                FilterOp::NotEquals("10".into()),
                ids(&["a", "empty", "null"]),
            ),
            // Numeric, not lexical: "10" sorts before "9" as text.
            (FilterOp::GreaterThan("9".into()), ids(&["b"])),
            (FilterOp::LessThan("2".into()), ids(&["a"])),
            (FilterOp::GreaterThanOrEqual("2.5".into()), ids(&["a", "b"])),
            (FilterOp::LessThanOrEqual("1".into()), ids(&["a"])),
            (
                FilterOp::In(vec!["1".into(), "10".into()]),
                ids(&["a", "b"]),
            ),
            (
                FilterOp::NotIn(vec!["1".into(), "10".into()]),
                ids(&["empty", "null"]),
            ),
            (FilterOp::Contains("5".into()), ids(&["a"])),
            (FilterOp::Exists, ids(&["a", "b"])),
            (FilterOp::NotExists, ids(&["empty", "null"])),
        ]
    }

    /// Regression: a filter on a scalar has-many list compared the whole
    /// stored JSON text, so `tags equals "a"` never matched `["a","b"]`. Each
    /// operator now reads the list element by element — on a hit, a miss, an
    /// empty list and a NULL column.
    #[test]
    fn scalar_has_many_filters_match_element_by_element() {
        let conn = list_db();

        for (op, expected) in text_cases() {
            assert_eq!(sql_ids(&conn, "tags", &op), expected, "tags {op:?}");
        }

        for (op, expected) in number_cases() {
            assert_eq!(sql_ids(&conn, "scores", &op), expected, "scores {op:?}");
        }
    }

    /// The in-memory evaluator (live events, population gating) decides every
    /// list filter exactly as SQL does — a scalar list, a has-many
    /// relationship's ids and a reference list inside a row alike.
    #[test]
    fn in_memory_list_filters_agree_with_sql() {
        let conn = list_db();

        let lists = [
            ("tags", text_cases()),
            ("scores", number_cases()),
            ("refs.id", text_cases()),
            ("items.related", text_cases()),
            ("content.links", text_cases()),
        ];

        for (field, cases) in lists {
            for (op, _) in cases {
                assert_eq!(
                    memory_ids(field, &op),
                    sql_ids(&conn, field, &op),
                    "{field} {op:?}"
                );
            }
        }
    }

    /// Regression: a has-many relationship filter ran every operator as "some
    /// junction row satisfies it", so `not_equals` matched a document that
    /// holds the id next to another one, and `not_exists` matched nothing. The
    /// junction rows now read as the elements of a list, agreeing with a
    /// scalar has-many list of the same ids for every operator.
    #[test]
    fn has_many_relationship_filters_agree_with_scalar_lists() {
        let conn = list_db();

        for (op, expected) in text_cases() {
            assert_eq!(sql_ids(&conn, "refs.id", &op), expected, "refs.id {op:?}");
        }
    }

    /// Regression: a has-many relationship inside an array or blocks row was
    /// compared as the whole stored text, so `items.related equals "a"` never
    /// matched `["a","b"]` and a polymorphic entry never matched its id. Its
    /// ids are now the list's elements — the id half of a polymorphic entry —
    /// read exactly as a top-level has-many relationship's.
    #[test]
    fn has_many_references_inside_rows_match_element_by_element() {
        let conn = list_db();

        for field in ["items.related", "content.links"] {
            for (op, expected) in text_cases() {
                assert_eq!(sql_ids(&conn, field, &op), expected, "{field} {op:?}");
                assert_eq!(memory_ids(field, &op), expected, "memory {field} {op:?}");
            }
        }
    }
}
