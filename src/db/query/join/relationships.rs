//! Has-many relationship and polymorphic join table operations.

use std::collections::HashMap;

use anyhow::Result;

use crate::db::query::helpers::join_table;
use crate::db::{DbConnection, DbValue};

use super::helpers::delete_junction_rows;

/// Set the related IDs for a has-many relationship junction table.
/// Deletes all existing rows for the parent (scoped by locale if provided) and inserts new ones.
///
/// Inserts are batched into a single multi-row INSERT to minimize round-trips.
///
/// **Must be called within a transaction.** The DELETE + INSERT sequence is not atomic on its own;
/// without a wrapping transaction, a failed INSERT leaves the relationship in an inconsistent state.
///
/// # Errors
///
/// Returns a backend error if the DELETE or INSERT fails.
pub fn set_related_ids(
    conn: &dyn DbConnection,
    collection: &str,
    field: &str,
    parent_id: &str,
    ids: &[String],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = join_table(collection, field);

    delete_junction_rows(conn, &table_name, parent_id, locale)?;

    if ids.is_empty() {
        return Ok(());
    }

    if let Some(loc) = locale {
        let cols_per_row = 4;
        let mut params: Vec<DbValue> = Vec::with_capacity(ids.len() * cols_per_row);
        let mut value_groups: Vec<String> = Vec::with_capacity(ids.len());

        for (i, id) in ids.iter().enumerate() {
            let base = i * cols_per_row;
            let p1 = conn.placeholder(base + 1);
            let p2 = conn.placeholder(base + 2);
            let p3 = conn.placeholder(base + 3);
            let p4 = conn.placeholder(base + 4);
            value_groups.push(format!("({p1}, {p2}, {p3}, {p4})"));

            params.push(DbValue::Text(parent_id.to_string()));
            params.push(DbValue::Text(id.clone()));
            params.push(DbValue::Integer(i64::try_from(i).unwrap_or(i64::MAX)));
            params.push(DbValue::Text(loc.to_string()));
        }

        let sql = format!(
            "INSERT INTO \"{}\" (parent_id, related_id, _order, _locale) VALUES {}",
            table_name,
            value_groups.join(", ")
        );
        conn.execute(&sql, &params)?;
    } else {
        let cols_per_row = 3;
        let mut params: Vec<DbValue> = Vec::with_capacity(ids.len() * cols_per_row);
        let mut value_groups: Vec<String> = Vec::with_capacity(ids.len());

        for (i, id) in ids.iter().enumerate() {
            let base = i * cols_per_row;
            let p1 = conn.placeholder(base + 1);
            let p2 = conn.placeholder(base + 2);
            let p3 = conn.placeholder(base + 3);
            value_groups.push(format!("({p1}, {p2}, {p3})"));

            params.push(DbValue::Text(parent_id.to_string()));
            params.push(DbValue::Text(id.clone()));
            params.push(DbValue::Integer(i64::try_from(i).unwrap_or(i64::MAX)));
        }

        let sql = format!(
            "INSERT INTO \"{}\" (parent_id, related_id, _order) VALUES {}",
            table_name,
            value_groups.join(", ")
        );
        conn.execute(&sql, &params)?;
    }

    Ok(())
}

/// Find related IDs for a has-many relationship junction table, ordered.
/// When `locale` is Some, filters by `_locale`.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn find_related_ids(
    conn: &dyn DbConnection,
    collection: &str,
    field: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<String>> {
    let table_name = join_table(collection, field);

    let (sql, params) = if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        (
            format!(
                "SELECT related_id FROM \"{table_name}\" WHERE parent_id = {p1} AND _locale = {p2} ORDER BY _order"
            ),
            vec![
                DbValue::Text(parent_id.to_string()),
                DbValue::Text(loc.to_string()),
            ],
        )
    } else {
        let p1 = conn.placeholder(1);
        (
            format!(
                "SELECT related_id FROM \"{table_name}\" WHERE parent_id = {p1} ORDER BY _order"
            ),
            vec![DbValue::Text(parent_id.to_string())],
        )
    };

    let rows = conn.query_all(&sql, &params)?;
    let ids = rows
        .into_iter()
        .filter_map(|row| row.opt_text_at(0))
        .collect();
    Ok(ids)
}

/// Set related items for a polymorphic has-many relationship junction table.
/// Each item is a `(related_collection, related_id)` pair.
/// Deletes all existing rows for the parent (scoped by locale if provided) and inserts new ones.
///
/// Inserts are batched into a single multi-row INSERT to minimize round-trips.
///
/// **Must be called within a transaction.** The DELETE + INSERT sequence is not atomic on its own;
/// without a wrapping transaction, a failed INSERT leaves the relationship in an inconsistent state.
///
/// # Errors
///
/// Returns a backend error if the DELETE or INSERT fails.
pub fn set_polymorphic_related(
    conn: &dyn DbConnection,
    collection: &str,
    field: &str,
    parent_id: &str,
    items: &[(String, String)],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = join_table(collection, field);

    delete_junction_rows(conn, &table_name, parent_id, locale)?;

    if items.is_empty() {
        return Ok(());
    }

    if let Some(loc) = locale {
        let cols_per_row = 5;
        let mut params: Vec<DbValue> = Vec::with_capacity(items.len() * cols_per_row);
        let mut value_groups: Vec<String> = Vec::with_capacity(items.len());

        for (i, (rel_col, rel_id)) in items.iter().enumerate() {
            let base = i * cols_per_row;
            let p1 = conn.placeholder(base + 1);
            let p2 = conn.placeholder(base + 2);
            let p3 = conn.placeholder(base + 3);
            let p4 = conn.placeholder(base + 4);
            let p5 = conn.placeholder(base + 5);
            value_groups.push(format!("({p1}, {p2}, {p3}, {p4}, {p5})"));

            params.push(DbValue::Text(parent_id.to_string()));
            params.push(DbValue::Text(rel_id.clone()));
            params.push(DbValue::Text(rel_col.clone()));
            params.push(DbValue::Integer(i64::try_from(i).unwrap_or(i64::MAX)));
            params.push(DbValue::Text(loc.to_string()));
        }

        let sql = format!(
            "INSERT INTO \"{}\" (parent_id, related_id, related_collection, _order, _locale) VALUES {}",
            table_name,
            value_groups.join(", ")
        );
        conn.execute(&sql, &params)?;
    } else {
        let cols_per_row = 4;
        let mut params: Vec<DbValue> = Vec::with_capacity(items.len() * cols_per_row);
        let mut value_groups: Vec<String> = Vec::with_capacity(items.len());

        for (i, (rel_col, rel_id)) in items.iter().enumerate() {
            let base = i * cols_per_row;
            let p1 = conn.placeholder(base + 1);
            let p2 = conn.placeholder(base + 2);
            let p3 = conn.placeholder(base + 3);
            let p4 = conn.placeholder(base + 4);
            value_groups.push(format!("({p1}, {p2}, {p3}, {p4})"));

            params.push(DbValue::Text(parent_id.to_string()));
            params.push(DbValue::Text(rel_id.clone()));
            params.push(DbValue::Text(rel_col.clone()));
            params.push(DbValue::Integer(i64::try_from(i).unwrap_or(i64::MAX)));
        }

        let sql = format!(
            "INSERT INTO \"{}\" (parent_id, related_id, related_collection, _order) VALUES {}",
            table_name,
            value_groups.join(", ")
        );
        conn.execute(&sql, &params)?;
    }

    Ok(())
}

/// Batched variant of [`find_related_ids`]. Issues a single
/// `SELECT … WHERE parent_id IN (…) ORDER BY parent_id, _order` for
/// every parent in `parent_ids` and groups the results by parent.
/// Parents with no related rows are absent from the returned map
/// (caller decides whether to fall back).
///
/// Replaces the N-roundtrip pattern in `hydrate_documents` for
/// has-many relationship fields: a `find` returning 10 docs with 3
/// relationship fields previously did 30 SELECTs; this batches it to
/// 3 (one per field).
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn find_related_ids_batch(
    conn: &dyn DbConnection,
    collection: &str,
    field: &str,
    parent_ids: &[&str],
    locale: Option<&str>,
) -> Result<HashMap<String, Vec<String>>> {
    if parent_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let table_name = join_table(collection, field);

    // Build "?1, ?2, …, ?N" for the IN clause. The trailing placeholder
    // (when locale is set) sits at position N+1.
    let in_placeholders = (1..=parent_ids.len())
        .map(|i| conn.placeholder(i))
        .collect::<Vec<_>>()
        .join(", ");

    let (sql, params) = if let Some(loc) = locale {
        let loc_ph = conn.placeholder(parent_ids.len() + 1);
        let sql = format!(
            "SELECT parent_id, related_id FROM \"{table_name}\" \
             WHERE parent_id IN ({in_placeholders}) AND _locale = {loc_ph} \
             ORDER BY parent_id, _order"
        );
        let mut params: Vec<DbValue> = parent_ids
            .iter()
            .map(|id| DbValue::Text((*id).to_string()))
            .collect();
        params.push(DbValue::Text(loc.to_string()));
        (sql, params)
    } else {
        let sql = format!(
            "SELECT parent_id, related_id FROM \"{table_name}\" \
             WHERE parent_id IN ({in_placeholders}) \
             ORDER BY parent_id, _order"
        );
        let params: Vec<DbValue> = parent_ids
            .iter()
            .map(|id| DbValue::Text((*id).to_string()))
            .collect();
        (sql, params)
    };

    let rows = conn.query_all(&sql, &params)?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let Some(parent) = row.opt_text_at(0) else {
            continue;
        };
        let Some(related) = row.opt_text_at(1) else {
            continue;
        };
        out.entry(parent).or_default().push(related);
    }
    Ok(out)
}

/// Find related items for a polymorphic has-many relationship junction table.
/// Returns `(related_collection, related_id)` pairs ordered by _order.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn find_polymorphic_related(
    conn: &dyn DbConnection,
    collection: &str,
    field: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let table_name = join_table(collection, field);

    let (sql, params) = if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        (
            format!(
                "SELECT related_collection, related_id FROM \"{table_name}\" WHERE parent_id = {p1} AND _locale = {p2} ORDER BY _order"
            ),
            vec![
                DbValue::Text(parent_id.to_string()),
                DbValue::Text(loc.to_string()),
            ],
        )
    } else {
        let p1 = conn.placeholder(1);
        (
            format!(
                "SELECT related_collection, related_id FROM \"{table_name}\" WHERE parent_id = {p1} ORDER BY _order"
            ),
            vec![DbValue::Text(parent_id.to_string())],
        )
    };

    let rows = conn.query_all(&sql, &params)?;
    let items = rows
        .into_iter()
        .filter_map(|row| Some((row.opt_text_at(0)?, row.opt_text_at(1)?)))
        .collect();
    Ok(items)
}

/// Batched variant of [`find_polymorphic_related`]. One SELECT for
/// every parent in `parent_ids`; results grouped by parent. Parents
/// with no rows are absent from the map.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn find_polymorphic_related_batch(
    conn: &dyn DbConnection,
    collection: &str,
    field: &str,
    parent_ids: &[&str],
    locale: Option<&str>,
) -> Result<HashMap<String, Vec<(String, String)>>> {
    if parent_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let table_name = join_table(collection, field);

    let in_placeholders = (1..=parent_ids.len())
        .map(|i| conn.placeholder(i))
        .collect::<Vec<_>>()
        .join(", ");

    let (sql, params) = if let Some(loc) = locale {
        let loc_ph = conn.placeholder(parent_ids.len() + 1);
        let sql = format!(
            "SELECT parent_id, related_collection, related_id FROM \"{table_name}\" \
             WHERE parent_id IN ({in_placeholders}) AND _locale = {loc_ph} \
             ORDER BY parent_id, _order"
        );
        let mut params: Vec<DbValue> = parent_ids
            .iter()
            .map(|id| DbValue::Text((*id).to_string()))
            .collect();
        params.push(DbValue::Text(loc.to_string()));
        (sql, params)
    } else {
        let sql = format!(
            "SELECT parent_id, related_collection, related_id FROM \"{table_name}\" \
             WHERE parent_id IN ({in_placeholders}) \
             ORDER BY parent_id, _order"
        );
        let params: Vec<DbValue> = parent_ids
            .iter()
            .map(|id| DbValue::Text((*id).to_string()))
            .collect();
        (sql, params)
    };

    let rows = conn.query_all(&sql, &params)?;
    let mut out: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for row in rows {
        let Some(parent) = row.opt_text_at(0) else {
            continue;
        };
        let Some(col) = row.opt_text_at(1) else {
            continue;
        };
        let Some(rel) = row.opt_text_at(2) else {
            continue;
        };
        out.entry(parent).or_default().push((col, rel));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::db::{BoxedConnection, pool};
    use tempfile::TempDir;

    fn setup_conn(sql: &str) -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(sql).unwrap();
        (dir, conn)
    }

    fn setup_junction_db() -> (TempDir, BoxedConnection) {
        setup_conn(
            "CREATE TABLE posts_tags (
                parent_id TEXT,
                related_id TEXT,
                _order INTEGER
            );",
        )
    }

    fn setup_polymorphic_db() -> (TempDir, BoxedConnection) {
        setup_conn(
            "CREATE TABLE posts_refs (
                parent_id TEXT,
                related_id TEXT,
                related_collection TEXT NOT NULL DEFAULT '',
                _order INTEGER,
                PRIMARY KEY (parent_id, related_id, related_collection)
            );",
        )
    }

    // ── set_related_ids + find_related_ids ───────────────────────────────────

    #[test]
    fn set_and_find_related_ids() {
        let (_dir, conn) = setup_junction_db();
        let ids = vec!["t1".to_string(), "t2".to_string(), "t3".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(
            found,
            vec!["t1", "t2", "t3"],
            "Should return IDs in insertion order"
        );
    }

    #[test]
    fn replace_related_ids() {
        let (_dir, conn) = setup_junction_db();
        let ids_old = vec!["t1".to_string(), "t2".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids_old, None).unwrap();

        let ids_new = vec!["t3".to_string(), "t4".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids_new, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(
            found,
            vec!["t3", "t4"],
            "Old IDs should be replaced by new ones"
        );
    }

    #[test]
    fn empty_related_ids() {
        let (_dir, conn) = setup_junction_db();
        let ids = vec!["t1".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids, None).unwrap();
        set_related_ids(&conn, "posts", "tags", "p1", &[], None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert!(
            found.is_empty(),
            "Should return empty list after setting empty IDs"
        );
    }

    #[test]
    fn set_and_find_related_ids_with_locale() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts_tags (
                parent_id TEXT,
                related_id TEXT,
                _order INTEGER,
                _locale TEXT
            );",
        );

        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["t1".to_string(), "t2".to_string()],
            Some("en"),
        )
        .unwrap();
        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["t3".to_string()],
            Some("de"),
        )
        .unwrap();

        let en = find_related_ids(&conn, "posts", "tags", "p1", Some("en")).unwrap();
        assert_eq!(en, vec!["t1", "t2"]);

        let de = find_related_ids(&conn, "posts", "tags", "p1", Some("de")).unwrap();
        assert_eq!(de, vec!["t3"]);

        // Replacing en should not affect de
        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["t4".to_string()],
            Some("en"),
        )
        .unwrap();
        let en = find_related_ids(&conn, "posts", "tags", "p1", Some("en")).unwrap();
        assert_eq!(en, vec!["t4"]);
        let de = find_related_ids(&conn, "posts", "tags", "p1", Some("de")).unwrap();
        assert_eq!(de, vec!["t3"]);
    }

    // ── set_polymorphic_related + find_polymorphic_related ───────────────────

    #[test]
    fn set_and_find_polymorphic_related() {
        let (_dir, conn) = setup_polymorphic_db();
        let items = vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
            ("articles".to_string(), "a2".to_string()),
        ];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &items, None).unwrap();

        let found = find_polymorphic_related(&conn, "posts", "refs", "p1", None).unwrap();
        assert_eq!(
            found,
            vec![
                ("articles".to_string(), "a1".to_string()),
                ("pages".to_string(), "pg1".to_string()),
                ("articles".to_string(), "a2".to_string()),
            ]
        );
    }

    #[test]
    fn replace_polymorphic_related() {
        let (_dir, conn) = setup_polymorphic_db();
        let old = vec![("articles".to_string(), "a1".to_string())];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &old, None).unwrap();

        let new_items = vec![
            ("pages".to_string(), "pg1".to_string()),
            ("pages".to_string(), "pg2".to_string()),
        ];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &new_items, None).unwrap();

        let found = find_polymorphic_related(&conn, "posts", "refs", "p1", None).unwrap();
        assert_eq!(
            found,
            vec![
                ("pages".to_string(), "pg1".to_string()),
                ("pages".to_string(), "pg2".to_string()),
            ]
        );
    }

    #[test]
    fn set_and_find_polymorphic_related_with_locale() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts_refs (
                parent_id TEXT,
                related_id TEXT,
                related_collection TEXT NOT NULL DEFAULT '',
                _order INTEGER,
                _locale TEXT
            );",
        );

        let items_en = vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
        ];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &items_en, Some("en")).unwrap();

        let items_de = vec![("articles".to_string(), "a2".to_string())];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &items_de, Some("de")).unwrap();

        let en = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("en")).unwrap();
        assert_eq!(en.len(), 2);
        assert_eq!(en[0], ("articles".to_string(), "a1".to_string()));

        let de = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("de")).unwrap();
        assert_eq!(de.len(), 1);
        assert_eq!(de[0], ("articles".to_string(), "a2".to_string()));

        // Replacing en should not affect de
        set_polymorphic_related(
            &conn,
            "posts",
            "refs",
            "p1",
            &[("pages".to_string(), "pg2".to_string())],
            Some("en"),
        )
        .unwrap();
        let en2 = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("en")).unwrap();
        assert_eq!(en2.len(), 1);
        assert_eq!(en2[0], ("pages".to_string(), "pg2".to_string()));
        let de2 = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("de")).unwrap();
        assert_eq!(de2.len(), 1, "de locale should be unchanged");
    }

    // ── batched variants ─────────────────────────────────────────────────────

    #[test]
    fn find_related_ids_batch_groups_per_parent() {
        let (_dir, conn) = setup_junction_db();
        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["t1".to_string(), "t2".to_string()],
            None,
        )
        .unwrap();
        set_related_ids(&conn, "posts", "tags", "p2", &["t3".to_string()], None).unwrap();
        // p3 has no tags — must be absent from the map, not present-empty.

        let grouped =
            find_related_ids_batch(&conn, "posts", "tags", &["p1", "p2", "p3"], None).unwrap();

        assert_eq!(
            grouped.get("p1"),
            Some(&vec!["t1".to_string(), "t2".to_string()])
        );
        assert_eq!(grouped.get("p2"), Some(&vec!["t3".to_string()]));
        assert!(!grouped.contains_key("p3"), "parents with no rows omitted");
    }

    #[test]
    fn find_related_ids_batch_preserves_order() {
        let (_dir, conn) = setup_junction_db();
        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &[
                "zebra".to_string(),
                "apple".to_string(),
                "mango".to_string(),
            ],
            None,
        )
        .unwrap();

        let grouped = find_related_ids_batch(&conn, "posts", "tags", &["p1"], None).unwrap();
        assert_eq!(
            grouped.get("p1"),
            Some(&vec![
                "zebra".to_string(),
                "apple".to_string(),
                "mango".to_string()
            ]),
            "insertion order via _order must survive the batched IN query"
        );
    }

    #[test]
    fn find_related_ids_batch_empty_input_returns_empty() {
        let (_dir, conn) = setup_junction_db();
        let grouped = find_related_ids_batch(&conn, "posts", "tags", &[], None).unwrap();
        assert!(grouped.is_empty());
    }

    #[test]
    fn find_related_ids_batch_filters_by_locale() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts_tags (
                parent_id TEXT,
                related_id TEXT,
                _order INTEGER,
                _locale TEXT
            );",
        );
        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["en_tag".to_string()],
            Some("en"),
        )
        .unwrap();
        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["de_tag".to_string()],
            Some("de"),
        )
        .unwrap();
        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p2",
            &["en_only".to_string()],
            Some("en"),
        )
        .unwrap();

        let en = find_related_ids_batch(&conn, "posts", "tags", &["p1", "p2"], Some("en")).unwrap();
        assert_eq!(en.get("p1"), Some(&vec!["en_tag".to_string()]));
        assert_eq!(en.get("p2"), Some(&vec!["en_only".to_string()]));

        let de = find_related_ids_batch(&conn, "posts", "tags", &["p1", "p2"], Some("de")).unwrap();
        assert_eq!(de.get("p1"), Some(&vec!["de_tag".to_string()]));
        assert!(!de.contains_key("p2"), "p2 has no de rows → omitted");
    }

    #[test]
    fn find_polymorphic_related_batch_groups_per_parent() {
        let (_dir, conn) = setup_polymorphic_db();
        set_polymorphic_related(
            &conn,
            "posts",
            "refs",
            "p1",
            &[
                ("articles".to_string(), "a1".to_string()),
                ("pages".to_string(), "pg1".to_string()),
            ],
            None,
        )
        .unwrap();
        set_polymorphic_related(
            &conn,
            "posts",
            "refs",
            "p2",
            &[("articles".to_string(), "a2".to_string())],
            None,
        )
        .unwrap();

        let grouped =
            find_polymorphic_related_batch(&conn, "posts", "refs", &["p1", "p2", "ghost"], None)
                .unwrap();

        let p1 = grouped.get("p1").expect("p1 has rows");
        assert!(p1.contains(&("articles".to_string(), "a1".to_string())));
        assert!(p1.contains(&("pages".to_string(), "pg1".to_string())));
        let p2 = grouped.get("p2").expect("p2 has rows");
        assert_eq!(p2, &vec![("articles".to_string(), "a2".to_string())]);
        assert!(!grouped.contains_key("ghost"));
    }
}
