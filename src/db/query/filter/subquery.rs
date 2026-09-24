//! EXISTS subqueries of filters on join tables — array and blocks rows and
//! has-many relationship/upload junctions.

use anyhow::{Result, anyhow, bail};

use super::{
    elements::{ListExpr, ListLeaf, build_list_condition, build_quantified},
    operators::{build_filter_condition, build_op_condition},
    resolve::{JsonLeaf, JsonStep, RowsLocale, SubqueryCondition},
};
use crate::core::{BLOCK_TYPE_KEY, FieldType};
use crate::db::{
    DbConnection, DbValue, Filter, FilterOp,
    query::{helpers::qualified_ident, is_valid_identifier},
};

/// Where a subquery filter looks: the join table, the parent table its rows
/// belong to, and the locale its rows are constrained to.
pub(super) struct SubqueryScope<'a> {
    pub(super) join_table: &'a str,
    pub(super) parent_table: &'a str,
    pub(super) rows_locale: Option<&'a RowsLocale>,
}

/// Generate the `EXISTS (SELECT 1 FROM … WHERE …)` clause of a subquery filter.
///
/// A has-many relationship's or upload's junction rows are its elements: the
/// filter quantifies over them exactly as a scalar has-many filter quantifies
/// over its list, so `not_equals` asks that no row hold the id (`NOT EXISTS`).
/// Array and blocks rows are records — the filter asks for some row whose
/// sub-field satisfies the operator, and a list inside that row — a scalar
/// has-many list, or a has-many reference's id list — then quantifies over its
/// own elements. Where block types define the path differently, a row is
/// tested with its own block type's reading (see [`json_subquery`]).
pub(super) fn build_subquery_sql(
    conn: &dyn DbConnection,
    scope: &SubqueryScope<'_>,
    condition: &SubqueryCondition,
    f: &Filter,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    match condition {
        SubqueryCondition::RelatedId => build_quantified(&f.op, |op| {
            let op_sql = related_id_condition(conn, &f.field, op, params)?;

            Ok(row_select(conn, scope, None, &op_sql, params))
        }),
        SubqueryCondition::Column {
            col,
            field_type,
            list,
        } => {
            let leaf = SubqueryLeaf {
                expr: col,
                field_type: field_type.as_ref(),
                list: list.as_ref(),
            };
            let op_sql = column_condition(conn, scope, &leaf, f, params)?;

            Ok(exists_row(conn, scope, None, &op_sql, params))
        }
        SubqueryCondition::BlockType => {
            let op_sql = build_op_condition(
                conn,
                &f.field,
                BLOCK_TYPE_KEY,
                &f.op,
                Some(&FieldType::Text),
                params,
            )?;

            Ok(exists_row(conn, scope, None, &op_sql, params))
        }
        SubqueryCondition::Json(leaves) => json_subquery(conn, scope, leaves, f, params),
    }
}

/// The value a subquery filter tests in each row: a join-table column or a
/// JSON extract, its field type, and the list it holds, if any.
struct SubqueryLeaf<'a> {
    expr: &'a str,
    field_type: Option<&'a FieldType>,
    list: Option<&'a ListLeaf>,
}

impl<'a> SubqueryLeaf<'a> {
    /// The value a reading inside a row's JSON tests.
    fn json(leaf: &'a JsonLeaf) -> Self {
        Self {
            expr: &leaf.extract_expr,
            field_type: leaf.field_type.as_ref(),
            list: leaf.list.as_ref(),
        }
    }
}

/// The subquery of a filter on a value inside a row's JSON.
///
/// A reading that holds in every row keeps its `json_each` expansions in the
/// row select's FROM. Readings of block types defining the path differently
/// are each tested only in rows of their block type, and nest their
/// expansions below that test (see [`typed_reading`]), so a row of another
/// type never has its value read — or expanded — as this type's. A row
/// matches when one reading holds: a negative operator stays inside its
/// reading, as it does in a row of a single block type (some row whose own
/// value satisfies it; a list inside the row read element by element).
///
/// A reading whose operand does not fit its field (text against a number)
/// matches none of its block type's rows; the operand is refused only when it
/// fits no declaring block type's reading — the absent reading of the other
/// types' rows (NULL, untyped) alone never makes a filter valid.
fn json_subquery(
    conn: &dyn DbConnection,
    scope: &SubqueryScope<'_>,
    leaves: &[JsonLeaf],
    f: &Filter,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    if let [leaf] = leaves
        && leaf.is_unconditional()
    {
        let op_sql = json_condition(conn, &SubqueryLeaf::json(leaf), f, params)?;
        let from = json_from(conn, scope.join_table, &leaf.each_joins());

        return Ok(exists_row(conn, scope, Some(&from), &op_sql, params));
    }

    let mut readings = Vec::new();
    let mut declared_reading = false;
    let mut first_error = None;

    for leaf in leaves {
        let bound = params.len();

        match typed_reading(conn, leaf, &leaf.steps, f, params) {
            Ok(sql) => {
                declared_reading |= !leaf.reads_absent();
                readings.push(sql);
            }
            Err(e) => {
                params.truncate(bound);
                first_error.get_or_insert(e);
            }
        }
    }

    if !declared_reading {
        return Err(first_error.unwrap_or_else(|| anyhow!("No reading of '{}'", f.field)));
    }

    let op_sql = format!("({})", readings.join(" OR "));

    Ok(exists_row(conn, scope, None, &op_sql, params))
}

/// The per-row test of one reading, `steps` at a time: a block-type step
/// guards everything below it — `CASE` evaluates the rest only for rows of
/// that type — and each expansion becomes its own `EXISTS` over the rows it
/// expands.
fn typed_reading(
    conn: &dyn DbConnection,
    leaf: &JsonLeaf,
    steps: &[JsonStep],
    f: &Filter,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    let Some((step, rest)) = steps.split_first() else {
        return json_condition(conn, &SubqueryLeaf::json(leaf), f, params);
    };

    match step {
        JsonStep::BlockType { expr, block_type } => {
            let type_ph = push_text(conn, block_type, params);
            let inner = typed_reading(conn, leaf, rest, f, params)?;

            Ok(format!(
                "CASE WHEN {expr} = {type_ph} THEN {inner} ELSE FALSE END"
            ))
        }
        JsonStep::OtherBlockType { expr, declared } => {
            let type_phs: Vec<String> = declared
                .iter()
                .map(|block_type| push_text(conn, block_type, params))
                .collect();
            let inner = typed_reading(conn, leaf, rest, f, params)?;

            Ok(format!(
                "CASE WHEN {expr} IS NULL OR {expr} NOT IN ({}) THEN {inner} ELSE FALSE END",
                type_phs.join(", ")
            ))
        }
        JsonStep::Each { source, alias } => {
            let inner = typed_reading(conn, leaf, rest, f, params)?;
            let rows = conn.json_each_source(source, alias);

            Ok(format!("EXISTS (SELECT 1 FROM {rows} WHERE {inner})"))
        }
    }
}

/// The per-row test of a has-many junction: the element operator on its
/// `related_id`, always text.
fn related_id_condition(
    conn: &dyn DbConnection,
    field: &str,
    op: &FilterOp,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    build_op_condition(
        conn,
        field,
        "\"related_id\"",
        op,
        Some(&FieldType::Text),
        params,
    )
}

/// The per-row test of an array sub-field column. The same validate-then-quote
/// guard the parent-table path applies, so a join-table column can't reach SQL
/// unchecked either; a list column is read qualified by its join table.
fn column_condition(
    conn: &dyn DbConnection,
    scope: &SubqueryScope<'_>,
    leaf: &SubqueryLeaf<'_>,
    f: &Filter,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    let Some(list) = leaf.list else {
        let column = Filter {
            field: leaf.expr.to_string(),
            op: f.op.clone(),
        };

        return build_filter_condition(conn, &column, &f.field, leaf.field_type, params);
    };

    if !is_valid_identifier(leaf.expr) {
        bail!(
            "Invalid field name '{}': must be alphanumeric/underscore",
            leaf.expr
        );
    }

    let list_expr = qualified_ident(Some(scope.join_table), leaf.expr);

    build_list_condition(conn, f, &ListExpr::new(&list_expr, list), params)
}

/// The per-row test of a value inside a row's JSON.
fn json_condition(
    conn: &dyn DbConnection,
    leaf: &SubqueryLeaf<'_>,
    f: &Filter,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    if let Some(list) = leaf.list {
        return build_list_condition(conn, f, &ListExpr::new(leaf.expr, list), params);
    }

    // A JSON extract is text on Postgres: a Number sub-field is cast so the
    // comparison is numeric, not lexical (or a type error), and a Checkbox
    // reads its stored `true`/`false` as the integer its operand binds as.
    let extract = match leaf.field_type {
        Some(FieldType::Number) => conn.json_number_cast(leaf.expr),
        Some(FieldType::Checkbox) => conn.json_checkbox_cast(leaf.expr),
        _ => leaf.expr.to_string(),
    };

    build_op_condition(conn, &f.field, &extract, &f.op, leaf.field_type, params)
}

/// The FROM list of a JSON subquery: the join table, then each `json_each`
/// expansion down to the filtered value.
fn json_from(conn: &dyn DbConnection, join_table: &str, each_joins: &[(&str, &str)]) -> String {
    let mut parts = vec![format!("\"{join_table}\"")];

    for (source, alias) in each_joins {
        parts.push(conn.json_each_source(source, alias));
    }

    parts.join(", ")
}

/// [`row_select`] wrapped in `EXISTS` — some row satisfies `op_sql`.
fn exists_row(
    conn: &dyn DbConnection,
    scope: &SubqueryScope<'_>,
    from: Option<&str>,
    op_sql: &str,
    params: &mut Vec<DbValue>,
) -> String {
    format!("EXISTS ({})", row_select(conn, scope, from, op_sql, params))
}

/// `SELECT 1` over the scope's rows of one parent document that satisfy
/// `op_sql`, constrained to the scope's locale.
///
/// `from` is the FROM list of a JSON subquery, whose `json_each` expansions
/// expose columns of their own — its `parent_id` is qualified by the join
/// table there. `None` reads the join table alone.
fn row_select(
    conn: &dyn DbConnection,
    scope: &SubqueryScope<'_>,
    from: Option<&str>,
    op_sql: &str,
    params: &mut Vec<DbValue>,
) -> String {
    let SubqueryScope {
        join_table,
        parent_table,
        ..
    } = *scope;
    let locale_sql = append_locale_clause(conn, scope, params);

    let (from, parent_id) = match from {
        Some(from) => (from.to_string(), format!("\"{join_table}\".parent_id")),
        None => (format!("\"{join_table}\""), "parent_id".to_string()),
    };

    format!(
        "SELECT 1 FROM {from} WHERE {parent_id} = \"{parent_table}\".id AND {op_sql}{locale_sql}"
    )
}

/// Produce the trailing locale fragment of a localized join table's rows and
/// push its bind parameters, or return `""` when the rows carry no locale.
///
/// A filter on a localized junction table (an array, blocks or has-many
/// relationship whose field is localized) matches exactly the rows the read
/// shows: the reading locale's, or — with fallback on — the fallback locale's
/// for a document holding no row in the reading locale, as hydration falls
/// back per document.
fn append_locale_clause(
    conn: &dyn DbConnection,
    scope: &SubqueryScope<'_>,
    params: &mut Vec<DbValue>,
) -> String {
    let Some(rows_locale) = scope.rows_locale else {
        return String::new();
    };

    let locale_col = format!("\"{}\"._locale", scope.join_table);
    let locale_ph = push_text(conn, &rows_locale.locale, params);

    let Some(fallback) = rows_locale.fallback.as_deref() else {
        return format!(" AND {locale_col} = {locale_ph}");
    };

    let fallback_ph = push_text(conn, fallback, params);
    let no_locale_rows = no_locale_rows_sql(conn, scope, &rows_locale.locale, params);

    format!(
        " AND ({locale_col} = {locale_ph} OR ({locale_col} = {fallback_ph} AND {no_locale_rows}))"
    )
}

/// `NOT EXISTS` over the parent document's rows in `locale` — the document
/// holds none, so its read shows the fallback locale's rows.
fn no_locale_rows_sql(
    conn: &dyn DbConnection,
    scope: &SubqueryScope<'_>,
    locale: &str,
    params: &mut Vec<DbValue>,
) -> String {
    let locale_ph = push_text(conn, locale, params);

    format!(
        "NOT EXISTS (SELECT 1 FROM \"{}\" AS crap_loc WHERE crap_loc.parent_id = \"{}\".id AND crap_loc._locale = {locale_ph})",
        scope.join_table, scope.parent_table
    )
}

/// Bind `value` as text and return its placeholder.
fn push_text(conn: &dyn DbConnection, value: &str, params: &mut Vec<DbValue>) -> String {
    params.push(DbValue::Text(value.to_string()));

    conn.placeholder(params.len())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::{InMemoryConn, query::filter::localized_rows_fixture};

    fn scope(rows_locale: Option<&RowsLocale>) -> SubqueryScope<'_> {
        SubqueryScope {
            join_table: "posts_items",
            parent_table: "posts",
            rows_locale,
        }
    }

    fn filter(field: &str, op: FilterOp) -> Filter {
        Filter {
            field: field.to_string(),
            op,
        }
    }

    /// A negative operator on a has-many junction asks that no row hold the
    /// id; the locale constraint binds after the operand, inside the subquery.
    #[test]
    fn a_negative_related_id_filter_is_not_exists() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();

        let sql = build_subquery_sql(
            &conn,
            &scope(Some(&RowsLocale::new("de", None))),
            &SubqueryCondition::RelatedId,
            &filter("tags.id", FilterOp::NotEquals("t1".into())),
            &mut params,
        )
        .unwrap();

        assert_eq!(
            sql,
            "NOT EXISTS (SELECT 1 FROM \"posts_items\" WHERE parent_id = \"posts\".id AND \"related_id\" = ?1 AND \"posts_items\"._locale = ?2)"
        );
        assert_eq!(
            params,
            vec![DbValue::Text("t1".into()), DbValue::Text("de".into())]
        );
    }

    /// With fallback on, a document's rows match in the reading locale — or in
    /// the fallback locale when it holds no row in the reading one, the rows
    /// hydration shows for it.
    #[test]
    fn a_fallback_locale_matches_the_rows_of_documents_without_the_reading_locale() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let rows_locale = RowsLocale::new("de", Some("en"));

        let sql = build_subquery_sql(
            &conn,
            &scope(Some(&rows_locale)),
            &SubqueryCondition::RelatedId,
            &filter("tags.id", FilterOp::Equals("t1".into())),
            &mut params,
        )
        .unwrap();

        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM \"posts_items\" WHERE parent_id = \"posts\".id AND \"related_id\" = ?1 \
             AND (\"posts_items\"._locale = ?2 OR (\"posts_items\"._locale = ?3 AND NOT EXISTS \
             (SELECT 1 FROM \"posts_items\" AS crap_loc WHERE crap_loc.parent_id = \"posts\".id AND crap_loc._locale = ?4))))"
        );
        assert_eq!(
            params,
            vec![
                DbValue::Text("t1".into()),
                DbValue::Text("de".into()),
                DbValue::Text("en".into()),
                DbValue::Text("de".into()),
            ]
        );
    }

    /// Every operator on a localized junction or array matches exactly the
    /// documents whose shown rows satisfy it — under fallback, without it, and
    /// for an all-locales read.
    #[test]
    fn localized_row_filters_match_the_rows_the_read_shows() {
        let conn = InMemoryConn::open();

        localized_rows_fixture::assert_filters_match_the_shown_rows(&conn, "posts");
    }

    /// A scalar has-many sub-field of an array row: some row whose own list
    /// matches, the list read qualified by the join table.
    #[test]
    fn an_array_list_column_quantifies_inside_the_row() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let condition = SubqueryCondition::Column {
            col: "tags".to_string(),
            field_type: Some(FieldType::Text),
            list: Some(ListLeaf::Scalar(FieldType::Text)),
        };

        let sql = build_subquery_sql(
            &conn,
            &scope(None),
            &condition,
            &filter("items.tags", FilterOp::Equals("a".into())),
            &mut params,
        )
        .unwrap();

        assert!(
            sql.starts_with(
                "EXISTS (SELECT 1 FROM \"posts_items\" WHERE parent_id = \"posts\".id AND EXISTS (SELECT 1 FROM json_each(\"posts_items\".\"tags\") AS crap_el"
            ),
            "{sql}"
        );
        assert!(
            sql.ends_with("AS crap_el WHERE crap_el.value = ?1))"),
            "{sql}"
        );
    }

    /// A scalar has-many value inside a block's JSON expands the extracted list.
    #[test]
    fn a_json_list_value_expands_the_extracted_array() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let condition = SubqueryCondition::Json(vec![JsonLeaf {
            steps: vec![],
            extract_expr: "json_extract(data, '$.tags')".to_string(),
            field_type: Some(FieldType::Number),
            list: Some(ListLeaf::Scalar(FieldType::Number)),
        }]);

        let sql = build_subquery_sql(
            &conn,
            &scope(None),
            &condition,
            &filter("content.tags", FilterOp::NotIn(vec!["1".into()])),
            &mut params,
        )
        .unwrap();

        assert!(
            sql.starts_with(
                "EXISTS (SELECT 1 FROM \"posts_items\" WHERE \"posts_items\".parent_id = \"posts\".id AND NOT EXISTS (SELECT 1 FROM json_each(json_extract(data, '$.tags')) AS crap_el"
            ),
            "{sql}"
        );
        assert_eq!(params, vec![DbValue::Real(1.0)]);
    }

    /// Rows of an array are records, not list elements: a negative operator
    /// on a plain sub-field still asks for some row that satisfies it.
    #[test]
    fn a_plain_array_sub_field_keeps_the_some_row_reading() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let condition = SubqueryCondition::Column {
            col: "name".to_string(),
            field_type: Some(FieldType::Text),
            list: None,
        };

        let sql = build_subquery_sql(
            &conn,
            &scope(None),
            &condition,
            &filter("items.name", FilterOp::NotEquals("x".into())),
            &mut params,
        )
        .unwrap();

        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM \"posts_items\" WHERE parent_id = \"posts\".id AND \"name\" != ?1)"
        );
    }

    fn content_scope() -> SubqueryScope<'static> {
        SubqueryScope {
            join_table: "posts_content",
            parent_table: "posts",
            rows_locale: None,
        }
    }

    fn block_type_step(expr: &str, block_type: &str) -> JsonStep {
        JsonStep::BlockType {
            expr: expr.to_string(),
            block_type: block_type.to_string(),
        }
    }

    /// `score` is a number in `stat` blocks and text in `note` blocks, read
    /// at `base`'s JSON after `steps` for each.
    fn score_readings(steps: &[JsonStep], row_type: &str, base: &str) -> Vec<JsonLeaf> {
        [("stat", FieldType::Number), ("note", FieldType::Text)]
            .into_iter()
            .map(|(block_type, field_type)| {
                let mut leaf_steps = steps.to_vec();
                leaf_steps.push(block_type_step(row_type, block_type));

                JsonLeaf {
                    steps: leaf_steps,
                    extract_expr: format!("json_extract({base}, '$.score')"),
                    field_type: Some(field_type),
                    list: None,
                }
            })
            .collect()
    }

    /// Readings of block types defining a path differently are each tested
    /// only in rows of their own type, the type bound before the operand.
    #[test]
    fn per_block_type_readings_test_each_row_by_its_type() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let leaves = score_readings(&[], "posts_content._block_type", "posts_content.data");

        let sql = build_subquery_sql(
            &conn,
            &content_scope(),
            &SubqueryCondition::Json(leaves),
            &filter("content.score", FilterOp::Equals("10".into())),
            &mut params,
        )
        .unwrap();

        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM \"posts_content\" WHERE parent_id = \"posts\".id AND \
             (CASE WHEN posts_content._block_type = ?1 THEN json_extract(posts_content.data, '$.score') = ?2 ELSE FALSE END \
             OR CASE WHEN posts_content._block_type = ?3 THEN json_extract(posts_content.data, '$.score') = ?4 ELSE FALSE END))"
        );
        assert_eq!(
            params,
            vec![
                DbValue::Text("stat".into()),
                DbValue::Real(10.0),
                DbValue::Text("note".into()),
                DbValue::Text("10".into()),
            ]
        );
    }

    /// Below a block-type test, a nested value's rows are expanded inside
    /// it, so a row of another type never has its value expanded.
    #[test]
    fn a_nested_reading_expands_below_its_block_type_test() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let expand = JsonStep::Each {
            source: "json_extract(posts_content.data, '$.nested')".to_string(),
            alias: "j0".to_string(),
        };
        let leaves = score_readings(
            &[expand],
            "json_extract(j0.value, '$._block_type')",
            "j0.value",
        );

        let sql = build_subquery_sql(
            &conn,
            &content_scope(),
            &SubqueryCondition::Json(leaves),
            &filter("content.nested.score", FilterOp::Equals("10".into())),
            &mut params,
        )
        .unwrap();

        assert!(
            sql.contains(
                "(EXISTS (SELECT 1 FROM json_each(json_extract(posts_content.data, '$.nested')) AS j0 \
                 WHERE CASE WHEN json_extract(j0.value, '$._block_type') = ?1 \
                 THEN json_extract(j0.value, '$.score') = ?2 ELSE FALSE END) OR EXISTS"
            ),
            "{sql}"
        );
    }

    /// An operand that does not fit one block type's field matches none of
    /// that type's rows; it is refused only when it fits no reading.
    #[test]
    fn an_operand_fitting_one_reading_drops_the_others() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let leaves = score_readings(&[], "posts_content._block_type", "posts_content.data");

        let sql = build_subquery_sql(
            &conn,
            &content_scope(),
            &SubqueryCondition::Json(leaves.clone()),
            &filter("content.score", FilterOp::Equals("high".into())),
            &mut params,
        )
        .unwrap();

        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM \"posts_content\" WHERE parent_id = \"posts\".id AND \
             (CASE WHEN posts_content._block_type = ?1 THEN json_extract(posts_content.data, '$.score') = ?2 ELSE FALSE END))"
        );
        assert_eq!(
            params,
            vec![DbValue::Text("note".into()), DbValue::Text("high".into())]
        );

        let numbers: Vec<JsonLeaf> = leaves
            .into_iter()
            .map(|mut leaf| {
                leaf.field_type = Some(FieldType::Number);
                leaf
            })
            .collect();
        let mut params = Vec::new();

        let err = build_subquery_sql(
            &conn,
            &content_scope(),
            &SubqueryCondition::Json(numbers),
            &filter("content.score", FilterOp::Equals("high".into())),
            &mut params,
        )
        .unwrap_err();

        assert!(err.to_string().contains("not a valid number"), "{err}");
        assert!(params.is_empty());
    }

    /// The reading of rows whose block type declares none of the readings'
    /// types: the untyped NULL a shared definition reads in such a row.
    fn absent_reading() -> JsonLeaf {
        JsonLeaf {
            steps: vec![JsonStep::OtherBlockType {
                expr: "posts_content._block_type".to_string(),
                declared: vec!["note".to_string(), "stat".to_string()],
            }],
            extract_expr: "CAST(NULL AS TEXT)".to_string(),
            field_type: None,
            list: None,
        }
    }

    /// A row of a block type declaring no field of the name is tested as the
    /// NULL it reads — `not_exists` holds there.
    #[test]
    fn rows_of_other_block_types_are_tested_as_null() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let mut leaves = score_readings(&[], "posts_content._block_type", "posts_content.data");
        leaves.push(absent_reading());

        let sql = build_subquery_sql(
            &conn,
            &content_scope(),
            &SubqueryCondition::Json(leaves),
            &filter("content.score", FilterOp::NotExists),
            &mut params,
        )
        .unwrap();

        assert!(
            sql.ends_with(
                "OR CASE WHEN posts_content._block_type IS NULL OR posts_content._block_type \
                 NOT IN (?3, ?4) THEN CAST(NULL AS TEXT) IS NULL ELSE FALSE END))"
            ),
            "{sql}"
        );
        assert_eq!(
            &params[2..],
            &[DbValue::Text("note".into()), DbValue::Text("stat".into())]
        );
    }

    /// The absent reading takes any operand; it never validates one that fits
    /// no declaring block type's field.
    #[test]
    fn the_absent_reading_alone_does_not_accept_an_operand() {
        let conn = InMemoryConn::open();
        let mut leaves: Vec<JsonLeaf> =
            score_readings(&[], "posts_content._block_type", "posts_content.data")
                .into_iter()
                .map(|mut leaf| {
                    leaf.field_type = Some(FieldType::Number);
                    leaf
                })
                .collect();
        leaves.push(absent_reading());

        let err = build_subquery_sql(
            &conn,
            &content_scope(),
            &SubqueryCondition::Json(leaves),
            &filter("content.score", FilterOp::Equals("high".into())),
            &mut Vec::new(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("not a valid number"), "{err}");
    }
}
