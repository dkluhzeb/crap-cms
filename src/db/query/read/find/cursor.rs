//! Cursor-based keyset pagination for [`super::find`].
//!
//! When the cursor encodes `status_val` (composite ordering: `_status
//! ASC, sort_col DIR, id DIR` — see [`super::sort::apply_order_by`]),
//! the keyset becomes a nested OR: rows in a different `_status`
//! bucket plus rows in the same bucket past the inner sort/id keyset.
//! Pre-composite cursors fall back to the original single-column keyset.

use anyhow::{Result, bail};

use crate::db::query::cursor::{CursorData, SortDirection};
use crate::db::{DbConnection, DbValue};

/// Resolved sort configuration for cursor pagination.
pub(super) struct SortInfo<'a> {
    pub(super) col: &'a str,
    pub(super) dir: SortDirection,
    pub(super) using_before: bool,
}

/// Apply cursor-based keyset pagination to the SQL query.
pub(super) fn apply_cursor_keyset(
    conn: &dyn DbConnection,
    cursor: &CursorData,
    sort: &SortInfo<'_>,
    resolved_col: &str,
    sql: &mut String,
    has_where: &mut bool,
    params: &mut Vec<DbValue>,
) -> Result<()> {
    if cursor.sort_col != sort.col {
        bail!(
            "Cursor sort_col '{}' does not match query order_by '{}'",
            cursor.sort_col,
            sort.col
        );
    }

    let inner_op = match (sort.dir, sort.using_before) {
        (SortDirection::Desc, false) | (SortDirection::Asc, true) => "<",
        _ => ">",
    };
    let sort_val = DbValue::from(&cursor.sort_val);

    let inner = inner_keyset_clause(conn, resolved_col, inner_op, sort_val, &cursor.id, params);
    let clause = if let Some(status_val) = cursor.status_val.as_deref() {
        // Composite (_status, sort_col, id) keyset. The outer `_status`
        // direction tracks `apply_order_by` — `_status ASC` normally,
        // flipped to `_status DESC` under `using_before` — so `outer_op`
        // flips to match. `inner` is parenthesised so the implicit
        // `AND` precedence over `OR` doesn't pull `inner`'s right-hand
        // side outside the same-bucket clause.
        let ph_status = conn.placeholder(params.len() + 1);
        params.push(DbValue::Text(status_val.to_string()));
        let outer_op = if sort.using_before { "<" } else { ">" };

        format!("(_status {outer_op} {ph_status}) OR (_status = {ph_status} AND ({inner}))")
    } else {
        inner
    };

    let prefix = if *has_where { " AND " } else { " WHERE " };
    sql.push_str(&format!("{prefix}({clause})"));
    *has_where = true;

    Ok(())
}

/// Build the inner keyset condition (no leading `AND` / `WHERE`, no
/// surrounding parens). Returns the same shape regardless of NULL
/// handling so the caller can compose it with an outer `_status`
/// clause uniformly, or wrap it on its own.
fn inner_keyset_clause(
    conn: &dyn DbConnection,
    col: &str,
    op: &str,
    sort_val: DbValue,
    cursor_id: &str,
    params: &mut Vec<DbValue>,
) -> String {
    if matches!(sort_val, DbValue::Null) {
        let ph_id = conn.placeholder(params.len() + 1);
        params.push(DbValue::Text(cursor_id.to_string()));

        if op == ">" {
            format!("({col} IS NULL AND id > {ph_id}) OR {col} IS NOT NULL")
        } else {
            format!("{col} IS NULL AND id < {ph_id}")
        }
    } else {
        let ph1 = conn.placeholder(params.len() + 1);
        let ph2 = conn.placeholder(params.len() + 2);
        params.push(sort_val);
        params.push(DbValue::Text(cursor_id.to_string()));

        format!("({col} {op} {ph1}) OR ({col} = {ph1} AND id {op} {ph2})")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::config::{CrapConfig, DatabaseConfig};
    use crate::core::DocumentFields;
    use crate::core::collection::CollectionDefinition;
    use crate::core::field::*;
    use crate::db::query::cursor::{CursorData, SortDirection};
    use crate::db::query::read::find::find;
    use crate::db::query::read::find::test_helpers::*;
    use crate::db::query::{SortValue, cursor::build_cursors, write::create};
    use crate::db::{DbConnection, DbValue, Filter, FilterClause, FilterOp, FindQuery, pool};

    #[test]
    fn cursor_and_offset_mutual_exclusion() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let query = FindQuery::builder()
            .after_cursor(Some(CursorData {
                sort_col: "id".to_string(),
                sort_dir: SortDirection::Asc,
                sort_val: SortValue::from(&json!("abc")),
                id: "abc".to_string(),
                ..Default::default()
            }))
            .offset(Some(10))
            .build();
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("mutually exclusive")
        );
    }

    #[test]
    fn cursor_asc_pagination() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        // Insert 5 rows with deterministic titles
        for i in 1..=5 {
            let mut data = DocumentFields::new();
            data.insert("title".to_string(), Value::String(format!("Post {:02}", i)));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // First page: limit=2, order by title ASC
        let q1 = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .build();
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].get_str("title"), Some("Post 01"));
        assert_eq!(page1[1].get_str("title"), Some("Post 02"));

        // Second page via cursor from last doc of page 1
        let last = &page1[1];
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::from(&json!(last.get_str("title").unwrap())),
            id: last.id.to_string(),
            ..Default::default()
        };
        let q2 = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .after_cursor(Some(cursor))
            .build();
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].get_str("title"), Some("Post 03"));
        assert_eq!(page2[1].get_str("title"), Some("Post 04"));

        // Third page
        let last2 = &page2[1];
        let cursor2 = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::from(&json!(last2.get_str("title").unwrap())),
            id: last2.id.to_string(),
            ..Default::default()
        };
        let q3 = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .after_cursor(Some(cursor2))
            .build();
        let page3 = find(&conn, "posts", &def, &q3, None).unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].get_str("title"), Some("Post 05"));
    }

    #[test]
    fn cursor_desc_pagination() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        for i in 1..=4 {
            let mut data = DocumentFields::new();
            data.insert("title".to_string(), Value::String(format!("Post {:02}", i)));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // First page DESC
        let q1 = FindQuery::builder()
            .order_by(Some("-title".to_string()))
            .limit(Some(2))
            .build();
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        assert_eq!(page1[0].get_str("title"), Some("Post 04"));
        assert_eq!(page1[1].get_str("title"), Some("Post 03"));

        // Second page via cursor
        let last = &page1[1];
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Desc,
            sort_val: SortValue::from(&json!(last.get_str("title").unwrap())),
            id: last.id.to_string(),
            ..Default::default()
        };
        let q2 = FindQuery::builder()
            .order_by(Some("-title".to_string()))
            .limit(Some(2))
            .after_cursor(Some(cursor))
            .build();
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].get_str("title"), Some("Post 02"));
        assert_eq!(page2[1].get_str("title"), Some("Post 01"));
    }

    #[test]
    fn cursor_wrong_sort_col_errors() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let query = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .after_cursor(Some(CursorData {
                sort_col: "status".to_string(),
                sort_dir: SortDirection::Asc,
                sort_val: SortValue::from(&json!("x")),
                id: "abc".to_string(),
                ..Default::default()
            }))
            .build();
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not match"));
    }

    #[test]
    fn before_cursor_asc_backward_pagination() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        for i in 1..=5 {
            let mut data = DocumentFields::new();
            data.insert("title".to_string(), Value::String(format!("Post {:02}", i)));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // Forward: get page 2 (Posts 03, 04) so we have a cursor to go backward from
        let p1q = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .build();
        let page1 = find(&conn, "posts", &def, &p1q, None).unwrap();
        let last_p1 = &page1[1];
        let fwd_cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::from(&json!(last_p1.get_str("title").unwrap())),
            id: last_p1.id.to_string(),
            ..Default::default()
        };
        let p2q = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .after_cursor(Some(fwd_cursor))
            .build();
        let page2 = find(&conn, "posts", &def, &p2q, None).unwrap();
        assert_eq!(page2[0].get_str("title"), Some("Post 03"));
        assert_eq!(page2[1].get_str("title"), Some("Post 04"));

        // Backward: from the first doc of page 2, go backward
        let first_p2 = &page2[0];
        let back_cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::from(&json!(first_p2.get_str("title").unwrap())),
            id: first_p2.id.to_string(),
            ..Default::default()
        };
        let bq = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .before_cursor(Some(back_cursor))
            .build();
        let back_page = find(&conn, "posts", &def, &bq, None).unwrap();

        // Should get Posts 01, 02 in correct ASC order
        assert_eq!(back_page.len(), 2);
        assert_eq!(back_page[0].get_str("title"), Some("Post 01"));
        assert_eq!(back_page[1].get_str("title"), Some("Post 02"));
    }

    #[test]
    fn before_cursor_desc_backward_pagination() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        for i in 1..=4 {
            let mut data = DocumentFields::new();
            data.insert("title".to_string(), Value::String(format!("Post {:02}", i)));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // Forward DESC page 1: Posts 04, 03
        let p1q = FindQuery::builder()
            .order_by(Some("-title".to_string()))
            .limit(Some(2))
            .build();
        let page1 = find(&conn, "posts", &def, &p1q, None).unwrap();
        assert_eq!(page1[0].get_str("title"), Some("Post 04"));
        assert_eq!(page1[1].get_str("title"), Some("Post 03"));

        // Forward DESC page 2: Posts 02, 01
        let last_p1 = &page1[1];
        let fwd_cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Desc,
            sort_val: SortValue::from(&json!(last_p1.get_str("title").unwrap())),
            id: last_p1.id.to_string(),
            ..Default::default()
        };
        let p2q = FindQuery::builder()
            .order_by(Some("-title".to_string()))
            .limit(Some(2))
            .after_cursor(Some(fwd_cursor))
            .build();
        let page2 = find(&conn, "posts", &def, &p2q, None).unwrap();
        assert_eq!(page2[0].get_str("title"), Some("Post 02"));
        assert_eq!(page2[1].get_str("title"), Some("Post 01"));

        // Backward from page 2 first doc → should get page 1 back
        let first_p2 = &page2[0];
        let back_cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Desc,
            sort_val: SortValue::from(&json!(first_p2.get_str("title").unwrap())),
            id: first_p2.id.to_string(),
            ..Default::default()
        };
        let bq = FindQuery::builder()
            .order_by(Some("-title".to_string()))
            .limit(Some(2))
            .before_cursor(Some(back_cursor))
            .build();
        let back_page = find(&conn, "posts", &def, &bq, None).unwrap();

        // Should get Posts 04, 03 in DESC order
        assert_eq!(back_page.len(), 2);
        assert_eq!(back_page[0].get_str("title"), Some("Post 04"));
        assert_eq!(back_page[1].get_str("title"), Some("Post 03"));
    }

    #[test]
    fn after_and_before_cursor_mutual_exclusion() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let cursor = CursorData {
            sort_col: "id".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::from(&json!("abc")),
            id: "abc".to_string(),
            ..Default::default()
        };
        let query = FindQuery::builder()
            .after_cursor(Some(cursor.clone()))
            .before_cursor(Some(cursor))
            .build();
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("mutually exclusive")
        );
    }

    #[test]
    fn cursor_sort_val_number_in_params() {
        // Numeric cursor pagination must use numeric comparison, not string.
        // With string comparison "9" > "10", so pagination would be wrong.
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        let conn = db_pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE scores (
                id TEXT PRIMARY KEY,
                name TEXT,
                points INTEGER,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("scores");
        def.fields = vec![
            FieldDefinition::builder("name", FieldType::Text).build(),
            FieldDefinition::builder("points", FieldType::Number).build(),
        ];

        // Insert rows with numeric values that would sort wrong as strings
        // String order: "10" < "5" < "9" (lexicographic)
        // Numeric order: 5 < 9 < 10 < 20 < 100
        let values = [
            (5, "five"),
            (9, "nine"),
            (10, "ten"),
            (20, "twenty"),
            (100, "hundred"),
        ];

        for (pts, name) in &values {
            conn.execute(
                "INSERT INTO scores (id, name, points, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                &[
                    DbValue::Text(format!("id-{name}")),
                    DbValue::Text(name.to_string()),
                    DbValue::Integer(*pts),
                    DbValue::Text("2026-01-01 00:00:00".into()),
                ],
            )
            .unwrap();
        }

        // Page 1: limit 2, order by points ASC → should get 5, 9
        let q1 = FindQuery::builder()
            .order_by(Some("points".to_string()))
            .limit(Some(2))
            .build();
        let page1 = find(&conn, "scores", &def, &q1, None).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].get_str("name"), Some("five"));
        assert_eq!(page1[1].get_str("name"), Some("nine"));

        // Page 2: cursor after points=9 → should get 10, 20 (NOT skip 10 as string "9" > "10")
        let cursor = CursorData {
            sort_col: "points".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::from(&json!(9)),
            id: "id-nine".to_string(),
            ..Default::default()
        };
        let q2 = FindQuery::builder()
            .order_by(Some("points".to_string()))
            .limit(Some(2))
            .after_cursor(Some(cursor))
            .build();
        let page2 = find(&conn, "scores", &def, &q2, None).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].get_str("name"), Some("ten"));
        assert_eq!(page2[1].get_str("name"), Some("twenty"));

        // Page 3: cursor after points=20 → should get 100
        let cursor2 = CursorData {
            sort_col: "points".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::from(&json!(20)),
            id: "id-twenty".to_string(),
            ..Default::default()
        };
        let q3 = FindQuery::builder()
            .order_by(Some("points".to_string()))
            .limit(Some(2))
            .after_cursor(Some(cursor2))
            .build();
        let page3 = find(&conn, "scores", &def, &q3, None).unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].get_str("name"), Some("hundred"));
    }

    #[test]
    fn cursor_sort_val_null_binds_as_null() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        // Null sort_val should execute without error (binds DbValue::Null, not empty string)
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::Null,
            id: "anyid".to_string(),
            ..Default::default()
        };
        let q = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .after_cursor(Some(cursor))
            .build();
        let result = find(&conn, "posts", &def, &q, None);
        assert!(result.is_ok());
    }

    #[test]
    fn cursor_sort_val_real_in_params() {
        // Verify f64 cursor values bind as DbValue::Real
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        let conn = db_pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE ratings (
                id TEXT PRIMARY KEY,
                label TEXT,
                score REAL,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("ratings");
        def.fields = vec![
            FieldDefinition::builder("label", FieldType::Text).build(),
            FieldDefinition::builder("score", FieldType::Number).build(),
        ];

        let values = [(1.5, "low"), (2.7, "mid"), (3.9, "high")];

        for (score, label) in &values {
            conn.execute(
                "INSERT INTO ratings (id, label, score, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                &[
                    DbValue::Text(format!("id-{label}")),
                    DbValue::Text(label.to_string()),
                    DbValue::Real(*score),
                    DbValue::Text("2026-01-01 00:00:00".into()),
                ],
            )
            .unwrap();
        }

        // Cursor after score=1.5 → should get mid, high
        let cursor = CursorData {
            sort_col: "score".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::from(&json!(1.5)),
            id: "id-low".to_string(),
            ..Default::default()
        };
        let q = FindQuery::builder()
            .order_by(Some("score".to_string()))
            .limit(Some(10))
            .after_cursor(Some(cursor))
            .build();
        let results = find(&conn, "ratings", &def, &q, None).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].get_str("label"), Some("mid"));
        assert_eq!(results[1].get_str("label"), Some("high"));
    }

    #[test]
    fn cursor_sort_val_bool_in_params() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        // Bool variant exercises the `other => other.to_string()` arm
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::from(&json!(true)), // Bool variant
            id: "anyid".to_string(),
            ..Default::default()
        };
        let q = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .after_cursor(Some(cursor))
            .build();
        let result = find(&conn, "posts", &def, &q, None);
        assert!(result.is_ok());
    }

    #[test]
    fn cursor_appended_to_existing_where_clause() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        // Insert some docs
        for i in 1..=3 {
            let mut data = DocumentFields::new();
            data.insert("title".to_string(), Value::String(format!("Post {:02}", i)));
            data.insert("status".to_string(), json!("active"));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // Use a filter (creates WHERE) plus cursor (appends AND condition).
        // Anchor id must sort after all nanoid chars ('~' = ASCII 126 > 'z' = 122)
        // so the tie-break condition `id > anchor` is always false for Post 01,
        // guaranteeing only strictly-after-title results are returned.
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::from(&json!("Post 01")),
            id: "~".to_string(),
            ..Default::default()
        };
        let q = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .filters(vec![FilterClause::Single(Filter {
                field: "status".to_string(),
                op: FilterOp::Equals("active".to_string()),
            })])
            .after_cursor(Some(cursor))
            .build();
        let result = find(&conn, "posts", &def, &q, None).unwrap();
        // All posts have status=active, but cursor anchors after "Post 01"
        assert!(
            result
                .iter()
                .all(|d| d.get_str("title").unwrap_or("") > "Post 01")
        );
    }

    #[test]
    fn valid_cursor_sort_col_succeeds() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let query = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .after_cursor(Some(CursorData {
                sort_col: "title".to_string(),
                sort_dir: SortDirection::Asc,
                sort_val: SortValue::Text("test".to_string()),
                id: "abc".to_string(),
                ..Default::default()
            }))
            .build();
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_ok());
    }

    #[test]
    fn cursor_forward_back_forward_consistent() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        // Insert 14 docs with unique sequential created_at (ISO format, matching DB storage)
        for i in 1..=14 {
            conn.execute(
                &format!(
                    "INSERT INTO posts (id, title, created_at, updated_at) VALUES ('d{:02}', 'Post {}', '2024-01-{:02}T12:00:00.000Z', '2024-01-{:02}T12:00:00.000Z')",
                    i, i, i, i
                ),
                &[],
            ).unwrap();
        }

        let limit = 10i64;

        // Page 1: initial load (no cursor, limit=10, default sort: -created_at)
        let q1 = FindQuery::builder().limit(Some(limit)).build();
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        assert_eq!(page1.len(), 10, "Page 1 should have 10 items");
        // DESC: newest first, so d14, d13, ..., d05
        assert_eq!(page1[0].id.as_ref(), "d14");
        assert_eq!(page1[9].id.as_ref(), "d05");

        // Page 2: forward with after_cursor (overfetch limit=11)
        let (_, end_cursor_p1) = build_cursors(&page1, "created_at", SortDirection::Desc, false);
        let end_cursor_data = CursorData::decode(end_cursor_p1.as_ref().unwrap()).unwrap();
        let q2 = FindQuery::builder()
            .limit(Some(limit + 1))
            .after_cursor(Some(end_cursor_data))
            .build();
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        let page2_count = page2.len().min(limit as usize);
        assert_eq!(page2_count, 4, "Page 2 should have 4 items");
        assert_eq!(page2[0].id.as_ref(), "d04");

        // Grab the start_cursor of page 2 for going back
        let page2_trimmed = &page2[..page2_count];
        let (start_cursor_p2, _) =
            build_cursors(page2_trimmed, "created_at", SortDirection::Desc, false);
        let start_cursor_data = CursorData::decode(start_cursor_p2.as_ref().unwrap()).unwrap();

        // Go back: before_cursor (overfetch limit=11)
        let q_back = FindQuery::builder()
            .limit(Some(limit + 1))
            .before_cursor(Some(start_cursor_data))
            .build();
        let page1_again = find(&conn, "posts", &def, &q_back, None).unwrap();
        // Trim overfetch from front (before_cursor extra is at index 0 after reversal)
        let page1_trimmed: Vec<_> = if page1_again.len() > limit as usize {
            page1_again[1..].to_vec()
        } else {
            page1_again
        };
        assert_eq!(
            page1_trimmed.len(),
            10,
            "Back to page 1 should have 10 items"
        );
        assert_eq!(
            page1_trimmed[0].id.as_ref(),
            "d14",
            "First item should be d14"
        );
        assert_eq!(
            page1_trimmed[9].id.as_ref(),
            "d05",
            "Last item should be d05"
        );

        // Forward again: end_cursor of the back-result
        let (_, end_cursor_p1_again) =
            build_cursors(&page1_trimmed, "created_at", SortDirection::Desc, false);
        let end_cursor_data_again =
            CursorData::decode(end_cursor_p1_again.as_ref().unwrap()).unwrap();
        let q2_again = FindQuery::builder()
            .limit(Some(limit + 1))
            .after_cursor(Some(end_cursor_data_again))
            .build();
        let page2_again = find(&conn, "posts", &def, &q2_again, None).unwrap();
        let page2_again_count = page2_again.len().min(limit as usize);
        assert_eq!(
            page2_again_count, page2_count,
            "Page 2 after back+forward should have same item count"
        );

        // Verify same IDs
        let ids_first: Vec<&str> = page2_trimmed.iter().map(|d| d.id.as_ref()).collect();
        let ids_second: Vec<&str> = page2_again[..page2_again_count]
            .iter()
            .map(|d| d.id.as_ref())
            .collect();
        assert_eq!(
            ids_first, ids_second,
            "Same documents should appear on page 2"
        );
    }
}
