//! Day-granular date filters.
//!
//! A date is stored as a UTC instant (`YYYY-MM-DDTHH:MM:SS.mmmZ`), while a
//! filter often names a calendar day (`2026-01-15`, what a date picker sends).
//! A bare-day operand on a Date field — or on the `created_at` / `updated_at`
//! timestamps — covers the whole UTC day `[D 00:00, D+1 00:00)`:
//!
//! | Operator | Matches |
//! |---|---|
//! | `equals` | on the day |
//! | `not_equals` | not on the day |
//! | `greater_than` | after the day (from the next midnight) |
//! | `greater_than_or_equal` | from the day's midnight |
//! | `less_than` | before the day's midnight |
//! | `less_than_or_equal` | before the next midnight |
//! | `in` / `not_in` | on (or not on) any listed day or instant |
//!
//! An operand carrying a time keeps its exact comparison. The SQL builder and
//! the in-memory evaluator both read a filter through [`day_filter`], so the
//! two decide every date filter alike.

use crate::db::{
    DbConnection, DbValue, FilterOp,
    query::helpers::{DayRange, normalize_date_value},
};

/// What a stored date is tested against for one operand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DateTest {
    /// Equal to this instant (an operand with a time, normalized).
    Exact(String),
    /// On this day.
    Within(DayRange),
    /// At or after this instant.
    AtLeast(String),
    /// Before this instant.
    Below(String),
}

impl DateTest {
    /// Whether the stored date `stored` passes the test, compared as text the
    /// way the SQL comparison compares the stored column.
    fn holds(&self, stored: &str) -> bool {
        match self {
            Self::Exact(instant) => stored == instant.as_str(),
            Self::Within(day) => stored >= day.start.as_str() && stored < day.end.as_str(),
            Self::AtLeast(instant) => stored >= instant.as_str(),
            Self::Below(instant) => stored < instant.as_str(),
        }
    }
}

/// A date filter with at least one bare-day operand: it matches when some of
/// its tests holds, or — `negated` — when none does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DayFilter {
    negated: bool,
    tests: Vec<DateTest>,
}

impl DayFilter {
    fn new(negated: bool, tests: Vec<DateTest>) -> Self {
        Self { negated, tests }
    }

    /// Whether a present, non-null stored date matches. A NULL date matches
    /// no day filter, negated ones included — the SQL comparison is NULL.
    pub(super) fn matches(&self, stored: &str) -> bool {
        let some = self.tests.iter().any(|test| test.holds(stored));

        some != self.negated
    }
}

/// How `op` reads a date when an operand names a whole day — `None` when none
/// does (or the operator takes no date operand), and the filter keeps its
/// exact comparison.
pub(super) fn day_filter(op: &FilterOp) -> Option<DayFilter> {
    match op {
        FilterOp::Equals(v) => on_day(false, v),
        FilterOp::NotEquals(v) => on_day(true, v),
        FilterOp::GreaterThan(v) => bound(v, |day| DateTest::AtLeast(day.end)),
        FilterOp::GreaterThanOrEqual(v) => bound(v, |day| DateTest::AtLeast(day.start)),
        FilterOp::LessThan(v) => bound(v, |day| DateTest::Below(day.start)),
        FilterOp::LessThanOrEqual(v) => bound(v, |day| DateTest::Below(day.end)),
        FilterOp::In(values) => membership(false, values),
        FilterOp::NotIn(values) => membership(true, values),
        _ => None,
    }
}

/// `equals` / `not_equals` a bare day.
fn on_day(negated: bool, operand: &str) -> Option<DayFilter> {
    let day = DayRange::of_operand(operand)?;

    Some(DayFilter::new(negated, vec![DateTest::Within(day)]))
}

/// An ordered comparison against a bare day, as the one bound it becomes.
fn bound(operand: &str, test: impl FnOnce(DayRange) -> DateTest) -> Option<DayFilter> {
    let day = DayRange::of_operand(operand)?;

    Some(DayFilter::new(false, vec![test(day)]))
}

/// `in` / `not_in` a list holding at least one bare day: each day covers its
/// day, each other operand keeps its exact comparison.
fn membership(negated: bool, operands: &[String]) -> Option<DayFilter> {
    if !operands.iter().any(|v| DayRange::of_operand(v).is_some()) {
        return None;
    }

    let tests = operands
        .iter()
        .map(|v| {
            DayRange::of_operand(v).map_or_else(
                || DateTest::Exact(normalize_date_value(v)),
                DateTest::Within,
            )
        })
        .collect();

    Some(DayFilter::new(negated, tests))
}

/// The SQL condition applying `filter` to the date expression `expr`,
/// appending its bind parameters to `params`. A negated filter is `NOT (…)`:
/// on a NULL date the condition is NULL, which no row passes — the reading an
/// exact `!=` / `NOT IN` gives it.
pub(super) fn build_day_condition(
    conn: &dyn DbConnection,
    expr: &str,
    filter: &DayFilter,
    params: &mut Vec<DbValue>,
) -> String {
    let mut bind = |value: &str| {
        params.push(DbValue::Text(value.to_string()));
        conn.placeholder(params.len())
    };

    let tests: Vec<String> = filter
        .tests
        .iter()
        .map(|test| match test {
            DateTest::Exact(instant) => format!("{expr} = {}", bind(instant)),
            DateTest::Within(day) => {
                let start = bind(&day.start);
                let end = bind(&day.end);

                format!("({expr} >= {start} AND {expr} < {end})")
            }
            DateTest::AtLeast(instant) => format!("{expr} >= {}", bind(instant)),
            DateTest::Below(instant) => format!("{expr} < {}", bind(instant)),
        })
        .collect();

    let any = tests.join(" OR ");

    match (filter.negated, tests.len()) {
        (true, _) => format!("NOT ({any})"),
        (false, 1) => any,
        (false, _) => format!("({any})"),
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::collections::HashMap;

    use serde_json::{Value, json};

    use super::*;
    use crate::{
        core::{BlockDefinition, DocumentFields, FieldDefinition, FieldType},
        db::{
            Filter, FilterClause, InMemoryConn,
            query::filter::{
                build_where_clause, memory::matches_constraints_typed,
                operators::build_op_condition,
            },
        },
    };

    const DAY: &str = "2026-01-15";

    fn render(op: &FilterOp) -> (String, Vec<DbValue>) {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let sql = build_op_condition(
            &conn,
            "due",
            "\"due\"",
            op,
            Some(&FieldType::Date),
            &mut params,
        )
        .unwrap();

        (sql, params)
    }

    fn text(values: &[&str]) -> Vec<DbValue> {
        values.iter().map(|v| DbValue::Text((*v).into())).collect()
    }

    const START: &str = "2026-01-15T00:00:00.000Z";
    const END: &str = "2026-01-16T00:00:00.000Z";

    /// Regression: a bare day was bound as the day's noon, so `equals` only
    /// matched a date stored at exactly 12:00. It now covers the whole day.
    #[test]
    fn equals_a_bare_day_covers_the_day() {
        let (sql, params) = render(&FilterOp::Equals(DAY.into()));

        assert_eq!(sql, "(\"due\" >= ?1 AND \"due\" < ?2)");
        assert_eq!(params, text(&[START, END]));
    }

    #[test]
    fn not_equals_a_bare_day_excludes_the_day() {
        let (sql, params) = render(&FilterOp::NotEquals(DAY.into()));

        assert_eq!(sql, "NOT ((\"due\" >= ?1 AND \"due\" < ?2))");
        assert_eq!(params, text(&[START, END]));
    }

    #[test]
    fn ordered_comparisons_take_the_day_boundaries() {
        let cases = [
            (FilterOp::GreaterThan(DAY.into()), "\"due\" >= ?1", END),
            (
                FilterOp::GreaterThanOrEqual(DAY.into()),
                "\"due\" >= ?1",
                START,
            ),
            (FilterOp::LessThan(DAY.into()), "\"due\" < ?1", START),
            (FilterOp::LessThanOrEqual(DAY.into()), "\"due\" < ?1", END),
        ];

        for (op, expected, bound) in cases {
            let (sql, params) = render(&op);

            assert_eq!(sql, expected, "{op:?}");
            assert_eq!(params, text(&[bound]), "{op:?}");
        }
    }

    /// A list mixing a bare day and an instant covers the day and keeps the
    /// instant exact.
    #[test]
    fn membership_mixes_days_and_instants() {
        let operands = vec![DAY.to_string(), "2026-02-01T09:30:00Z".to_string()];

        let (sql, params) = render(&FilterOp::In(operands.clone()));
        assert_eq!(sql, "((\"due\" >= ?1 AND \"due\" < ?2) OR \"due\" = ?3)");
        assert_eq!(params, text(&[START, END, "2026-02-01T09:30:00.000Z"]));

        let (sql, _) = render(&FilterOp::NotIn(operands));
        assert_eq!(
            sql,
            "NOT ((\"due\" >= ?1 AND \"due\" < ?2) OR \"due\" = ?3)"
        );
    }

    /// An operand with a time keeps its exact comparison, normalized to the
    /// stored form.
    #[test]
    fn an_operand_with_a_time_stays_exact() {
        let (sql, params) = render(&FilterOp::Equals("2026-01-15T09:00".into()));
        assert_eq!(sql, "\"due\" = ?1");
        assert_eq!(params, text(&["2026-01-15T09:00:00.000Z"]));

        let (sql, _) = render(&FilterOp::In(vec!["2026-01-15T09:00:00Z".into()]));
        assert_eq!(sql, "\"due\" IN (?1)");

        assert_eq!(day_filter(&FilterOp::Like(DAY.into())), None);
        assert_eq!(day_filter(&FilterOp::Contains(DAY.into())), None);
    }

    // ── SQL ↔ in-memory agreement ────────────────────────────────────────

    /// Stored instants around the filtered day: its first and last minutes,
    /// its noon (a `dayOnly` value), both neighbouring days' edges, and NULL.
    const ROWS: &[(&str, Option<&str>)] = &[
        ("before", Some("2026-01-14T23:59:59.999Z")),
        ("early", Some("2026-01-15T00:30:00.000Z")),
        ("noon", Some("2026-01-15T12:00:00.000Z")),
        ("late", Some("2026-01-15T23:59:00.000Z")),
        ("next", Some("2026-01-16T00:00:00.000Z")),
        ("null", None),
    ];

    fn fields() -> Vec<FieldDefinition> {
        let date = |name: &str| FieldDefinition::builder(name, FieldType::Date).build();
        let block = BlockDefinition::new("event", vec![date("at")]);

        vec![
            date("due"),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![date("due")])
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![date("due")])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![block])
                .build(),
        ]
    }

    /// A database holding [`ROWS`]: every date in a column, a group column,
    /// the system timestamps, an array row and a block.
    fn date_db() -> InMemoryConn {
        let conn = InMemoryConn::open();
        conn.setup(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, due TEXT, meta__due TEXT,
               created_at TEXT, updated_at TEXT);
             CREATE TABLE posts_items (id TEXT, parent_id TEXT, due TEXT);
             CREATE TABLE posts_content (id TEXT, parent_id TEXT,
               _block_type TEXT, data TEXT);",
        );

        for (id, date) in ROWS {
            let id_value = DbValue::Text((*id).into());
            let value = date.map_or(DbValue::Null, |d| DbValue::Text(d.into()));
            let data = json!({ "_block_type": "event", "at": date }).to_string();

            conn.execute(
                "INSERT INTO posts VALUES (?1, ?2, ?2, ?2, ?2)",
                &[id_value.clone(), value.clone()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO posts_items VALUES (?1, ?1, ?2)",
                &[id_value.clone(), value],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO posts_content VALUES (?1, ?1, 'event', ?2)",
                &[id_value, DbValue::Text(data)],
            )
            .unwrap();
        }

        conn
    }

    fn single(field: &str, op: &FilterOp) -> Vec<FilterClause> {
        vec![FilterClause::Single(Filter {
            field: field.to_string(),
            op: op.clone(),
        })]
    }

    /// The ids SQL matches for `field op`, sorted.
    fn sql_ids(conn: &InMemoryConn, field: &str, op: &FilterOp) -> Vec<String> {
        let mut params = Vec::new();
        let filters = single(field, op);
        let clause =
            build_where_clause(conn, &filters, "posts", &fields(), None, &mut params).unwrap();

        conn.query_all(
            &format!("SELECT id FROM posts{clause} ORDER BY id"),
            &params,
        )
        .unwrap()
        .iter()
        .map(|row| row.get_string("id").unwrap())
        .collect()
    }

    /// A row of [`ROWS`] as a read returns it.
    fn document(date: Option<&str>) -> DocumentFields {
        let value = date.map_or(Value::Null, |d| json!(d));

        DocumentFields::from(HashMap::from([
            ("due".to_string(), value.clone()),
            ("meta".to_string(), json!({ "due": value })),
            ("created_at".to_string(), value.clone()),
            ("updated_at".to_string(), value.clone()),
            ("items".to_string(), json!([{ "due": value }])),
            (
                "content".to_string(),
                json!([{ "_block_type": "event", "at": value }]),
            ),
        ]))
    }

    /// The ids the in-memory evaluator matches for `field op`, sorted.
    fn memory_ids(field: &str, op: &FilterOp) -> Vec<String> {
        let fields = fields();
        let filters = single(field, op);
        let mut ids: Vec<String> = ROWS
            .iter()
            .filter(|(_, date)| matches_constraints_typed(&document(*date), &filters, &fields))
            .map(|(id, _)| (*id).to_string())
            .collect();

        ids.sort();
        ids
    }

    fn ids(list: &[&str]) -> Vec<String> {
        let mut ids: Vec<String> = list.iter().map(|id| (*id).to_string()).collect();

        ids.sort();
        ids
    }

    fn cases() -> Vec<(FilterOp, Vec<String>)> {
        let day = || DAY.to_string();

        vec![
            (FilterOp::Equals(day()), ids(&["early", "noon", "late"])),
            (FilterOp::NotEquals(day()), ids(&["before", "next"])),
            (FilterOp::GreaterThan(day()), ids(&["next"])),
            (
                FilterOp::GreaterThanOrEqual(day()),
                ids(&["early", "noon", "late", "next"]),
            ),
            (FilterOp::LessThan(day()), ids(&["before"])),
            (
                FilterOp::LessThanOrEqual(day()),
                ids(&["before", "early", "noon", "late"]),
            ),
            (
                FilterOp::In(vec!["2026-01-14".into(), "2026-01-16T00:00:00Z".into()]),
                ids(&["before", "next"]),
            ),
            (
                FilterOp::NotIn(vec![day(), "2026-01-16T00:00:00Z".into()]),
                ids(&["before"]),
            ),
            // An operand with a time keeps its exact comparison.
            (
                FilterOp::Equals("2026-01-15T12:00:00Z".into()),
                ids(&["noon"]),
            ),
            (
                FilterOp::GreaterThan("2026-01-15T12:00".into()),
                ids(&["late", "next"]),
            ),
            (
                FilterOp::LessThanOrEqual("2026-01-15T00:30:00Z".into()),
                ids(&["before", "early"]),
            ),
        ]
    }

    /// Regression: a bare day matched only its noon on a Date field and no
    /// instant at all on `created_at` / `updated_at` (compared as text), so
    /// "on or before D" left out most of D and "after D" took it in. Every
    /// place a date lives — a column, a group, the system timestamps, an array
    /// row, a block — covers the whole day, in SQL and in memory alike.
    #[test]
    fn a_bare_day_covers_the_day_everywhere_a_date_lives() {
        let conn = date_db();

        for field in [
            "due",
            "meta__due",
            "created_at",
            "updated_at",
            "items.due",
            "content.at",
        ] {
            for (op, expected) in cases() {
                assert_eq!(sql_ids(&conn, field, &op), expected, "sql {field} {op:?}");
                assert_eq!(memory_ids(field, &op), expected, "memory {field} {op:?}");
            }
        }
    }
}
