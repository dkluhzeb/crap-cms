//! Blocks field join table operations.

use anyhow::Result;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

use crate::core::BLOCK_TYPE_KEY;
use crate::db::query::helpers::join_table;
use crate::db::{DbConnection, DbValue};

use super::helpers::{
    JunctionTarget, delete_junction_rows_except, select_junction_rows, select_junction_rows_batch,
};

/// Split a block row into `(_block_type, data_json)` for INSERT.
///
/// `_block_type` and `id` are stored in dedicated SQL columns; the
/// remainder of the row's keys are serialized to the `data` column as
/// JSON. Errors when `_block_type` is missing or non-string.
fn split_block_row(row: &Value, order: usize) -> Result<(String, String)> {
    let block_type = row
        .get(BLOCK_TYPE_KEY)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Block row at index {order} is missing '{BLOCK_TYPE_KEY}'"))?
        .to_string();

    let mut data_map = row.as_object().cloned().unwrap_or_default();
    data_map.remove(BLOCK_TYPE_KEY);
    data_map.remove("id");

    let data_json = Value::Object(data_map).to_string();

    Ok((block_type, data_json))
}

/// Build a matched block row's `(_block_type, data_json)`, merging the stored
/// row's top-level fields under the incoming ones. A field absent from the
/// incoming row — removed by the write-access strip, or not sent — keeps its
/// stored value; a present field (including an explicit null) overwrites. When
/// the block *type* changes on the same id there are no shared fields to
/// preserve, so the incoming row replaces the data wholesale.
///
/// Preservation is top-level only: everything inside a block is JSON, so a
/// write-denied leaf nested inside a block group/array follows the same
/// nested-JSON boundary as an array-in-array (replaced with its container).
fn merged_block_data(
    row: &Value,
    stored: Option<&Value>,
    order: usize,
) -> Result<(String, String)> {
    let block_type = row
        .get(BLOCK_TYPE_KEY)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Block row at index {order} is missing '{BLOCK_TYPE_KEY}'"))?
        .to_string();

    let same_type = stored
        .and_then(|s| s.get(BLOCK_TYPE_KEY))
        .and_then(|v| v.as_str())
        == Some(block_type.as_str());

    let mut data_map = if same_type {
        let mut m = stored
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        m.remove("id");
        m.remove(BLOCK_TYPE_KEY);
        m
    } else {
        Map::new()
    };

    if let Some(incoming) = row.as_object() {
        for (k, v) in incoming {
            if k == "id" || k == BLOCK_TYPE_KEY {
                continue;
            }
            data_map.insert(k.clone(), v.clone());
        }
    }

    Ok((block_type, Value::Object(data_map).to_string()))
}

/// INSERT a brand-new block row.
fn insert_block_row(
    conn: &dyn DbConnection,
    target: &JunctionTarget,
    id: &str,
    order: i64,
    block_type: &str,
    data_json: &str,
) -> Result<()> {
    let mut cols: Vec<&str> = vec!["id", "parent_id", "_order", "_block_type", "data"];
    let mut params: Vec<DbValue> = vec![
        DbValue::Text(id.to_string()),
        DbValue::Text(target.parent_id.to_string()),
        DbValue::Integer(order),
        DbValue::Text(block_type.to_string()),
        DbValue::Text(data_json.to_string()),
    ];

    if let Some(loc) = target.locale {
        cols.push("_locale");
        params.push(DbValue::Text(loc.to_string()));
    }

    let placeholders = (1..=params.len())
        .map(|i| conn.placeholder(i))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO \"{}\" ({}) VALUES ({placeholders})",
        target.table_name,
        cols.join(", ")
    );

    conn.execute(&sql, &params)?;
    Ok(())
}

/// UPDATE a matched block row in place — `_order`, `_block_type`, and the
/// already-merged `data` (see [`merged_block_data`]).
fn update_block_row(
    conn: &dyn DbConnection,
    table_name: &str,
    id: &str,
    order: i64,
    block_type: &str,
    data_json: &str,
) -> Result<()> {
    let (p1, p2, p3, p4) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
        conn.placeholder(4),
    );
    let sql = format!(
        "UPDATE \"{table_name}\" SET _order = {p1}, _block_type = {p2}, data = {p3} WHERE id = {p4}"
    );

    conn.execute(
        &sql,
        &[
            DbValue::Integer(order),
            DbValue::Text(block_type.to_string()),
            DbValue::Text(data_json.to_string()),
            DbValue::Text(id.to_string()),
        ],
    )?;
    Ok(())
}

/// Set block rows for a blocks field join table via a diff-based, per-row
/// write with top-level field preservation.
///
/// An incoming row that carries an `id` matching an existing row of this parent
/// (and locale) UPDATEs that row, keeping its stored top-level fields that the
/// incoming row does not supply (see [`merged_block_data`]) — so a write-denied
/// block field survives, as the scalar/array paths preserve an untouched
/// column. A row with no id, or an id that is not an existing row, INSERTs a
/// new row with a server-minted id. Rows the incoming set drops are deleted.
/// `_order` follows the incoming position. When `locale` is Some the whole diff
/// is scoped to that locale.
///
/// # Errors
///
/// Returns a backend error if any DELETE, UPDATE, or INSERT fails, or if a
/// block row is missing its `_block_type`.
pub fn set_block_rows(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    rows: &[Value],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = join_table(collection, field_name);

    // Stored rows keyed by id, so a matched row can merge its preserved fields.
    let stored: HashMap<String, Value> =
        find_block_rows(conn, collection, field_name, parent_id, locale)?
            .into_iter()
            .filter_map(|v| {
                let id = v.get("id")?.as_str()?.to_string();
                Some((id, v))
            })
            .collect();

    let mut keep: HashSet<String> = HashSet::with_capacity(rows.len());
    let mut planned: Vec<(String, bool, usize)> = Vec::with_capacity(rows.len());
    for (order, row) in rows.iter().enumerate() {
        let (id, is_update) = match row.get("id").and_then(Value::as_str) {
            Some(cid) if stored.contains_key(cid) && !keep.contains(cid) => (cid.to_string(), true),
            _ => (nanoid::nanoid!(), false),
        };
        keep.insert(id.clone());
        planned.push((id, is_update, order));
    }

    delete_junction_rows_except(conn, &table_name, parent_id, locale, &keep)?;

    let target = JunctionTarget {
        table_name: &table_name,
        parent_id,
        locale,
    };

    for ((id, is_update, order), row) in planned.into_iter().zip(rows.iter()) {
        let order_i64 = i64::try_from(order).unwrap_or(i64::MAX);

        if is_update {
            let (block_type, data_json) = merged_block_data(row, stored.get(&id), order)?;
            update_block_row(conn, &table_name, &id, order_i64, &block_type, &data_json)?;
        } else {
            let (block_type, data_json) = split_block_row(row, order)?;
            insert_block_row(conn, &target, &id, order_i64, &block_type, &data_json)?;
        }
    }

    Ok(())
}

/// Find block rows for a blocks field join table, ordered.
/// When `locale` is Some, filters by `_locale`.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn find_block_rows(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<Value>> {
    let table_name = join_table(collection, field_name);
    let (sql, params) = select_junction_rows(
        conn,
        &table_name,
        "id, _block_type, data",
        parent_id,
        locale,
    );

    let db_rows = conn.query_all(&sql, &params)?;
    let result = db_rows
        .iter()
        .filter_map(|row| decode_block_row(row, 0))
        .collect();
    Ok(result)
}

/// Decode one block join-table row starting at column `start`
/// (`id, _block_type, data`). Shared by the per-parent and batched readers.
fn decode_block_row(row: &crate::db::DbRow, start: usize) -> Option<Value> {
    let DbValue::Text(id_str) = row.get_value(start).cloned()? else {
        return None;
    };
    let DbValue::Text(bt_str) = row.get_value(start + 1).cloned()? else {
        return None;
    };
    let data_str = if let Some(DbValue::Text(s)) = row.get_value(start + 2).cloned() {
        s
    } else {
        String::new()
    };

    let mut map = match serde_json::from_str::<Value>(&data_str) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    map.insert("id".to_string(), Value::String(id_str));
    map.insert(BLOCK_TYPE_KEY.to_string(), Value::String(bt_str));
    Some(Value::Object(map))
}

/// Batched twin of [`find_block_rows`]: read the block rows of MANY parents
/// in one `IN (…)` query and bucket them per parent (ordered — the shared
/// batch SELECT orders by `parent_id, _order`). Parents with no rows are
/// absent from the map, which is what lets the caller's locale-fallback
/// pass target exactly the empty parents.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn find_block_rows_batch(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_ids: &[&str],
    locale: Option<&str>,
) -> Result<HashMap<String, Vec<Value>>> {
    if parent_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let table_name = join_table(collection, field_name);
    let (sql, params) = select_junction_rows_batch(
        conn,
        &table_name,
        "parent_id, id, _block_type, data",
        parent_ids,
        locale,
    );

    let db_rows = conn.query_all(&sql, &params)?;
    let mut out: HashMap<String, Vec<Value>> = HashMap::new();

    for row in &db_rows {
        let Some(DbValue::Text(parent)) = row.get_value(0).cloned() else {
            continue;
        };
        let Some(decoded) = decode_block_row(row, 1) else {
            continue;
        };
        out.entry(parent).or_default().push(decoded);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

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

    fn setup_blocks_db() -> (TempDir, BoxedConnection) {
        setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_content (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 _block_type TEXT,
                 data TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        )
    }

    // ── set_block_rows + find_block_rows ─────────────────────────────────────

    #[test]
    fn set_and_find_block_rows() {
        let (_dir, conn) = setup_blocks_db();
        let blocks = vec![
            json!({"_block_type": "paragraph", "text": "Hello world"}),
            json!({"_block_type": "image", "url": "/img/photo.jpg", "alt": "A photo"}),
        ];
        set_block_rows(&conn, "posts", "content", "p1", &blocks, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["_block_type"], "paragraph");
        assert_eq!(found[0]["text"], "Hello world");
        assert_eq!(found[1]["_block_type"], "image");
        assert_eq!(found[1]["url"], "/img/photo.jpg");
        assert_eq!(found[1]["alt"], "A photo");
        assert!(found[0]["id"].as_str().is_some(), "Block should have an id");
        assert!(found[1]["id"].as_str().is_some(), "Block should have an id");
    }

    #[test]
    fn replace_block_rows() {
        let (_dir, conn) = setup_blocks_db();
        let blocks_old = vec![json!({"_block_type": "paragraph", "text": "Old text"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_old, None).unwrap();

        let blocks_new = vec![json!({"_block_type": "heading", "level": 1, "text": "New heading"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_new, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(found.len(), 1, "Old blocks should be replaced");
        assert_eq!(found[0]["_block_type"], "heading");
        assert_eq!(found[0]["text"], "New heading");
    }

    /// The founding fix for blocks: an UPDATE matching a row by `id`, of the
    /// same block type, keeps the stored top-level fields the incoming row omits
    /// (as the write-access strip would drop a denied field) while overwriting
    /// the ones it supplies.
    #[test]
    fn set_block_rows_diff_preserves_omitted_field_on_matched_id() {
        let (_dir, conn) = setup_blocks_db();
        let seed = vec![json!({"_block_type": "hero", "title": "T", "subtitle": "S"})];
        set_block_rows(&conn, "posts", "content", "p1", &seed, None).unwrap();
        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        let id0 = found[0]["id"].as_str().unwrap().to_string();

        // Same block type, change `title`, omit `subtitle`.
        let update = vec![json!({"id": id0, "_block_type": "hero", "title": "T2"})];
        set_block_rows(&conn, "posts", "content", "p1", &update, None).unwrap();

        let after = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0]["id"].as_str().unwrap(), id0, "row keeps its id");
        assert_eq!(after[0]["title"], "T2", "supplied field updated");
        assert_eq!(after[0]["subtitle"], "S", "omitted field PRESERVED");
    }

    /// A changed `_block_type` on the same id has no shared fields, so the
    /// incoming row replaces the data wholesale — no stale field bleeds across
    /// block shapes.
    #[test]
    fn set_block_rows_diff_block_type_change_replaces_data() {
        let (_dir, conn) = setup_blocks_db();
        let seed = vec![json!({"_block_type": "hero", "title": "T"})];
        set_block_rows(&conn, "posts", "content", "p1", &seed, None).unwrap();
        let id0 = find_block_rows(&conn, "posts", "content", "p1", None).unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();

        let update = vec![json!({"id": id0, "_block_type": "quote", "text": "Q"})];
        set_block_rows(&conn, "posts", "content", "p1", &update, None).unwrap();

        let after = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(after[0]["_block_type"], "quote");
        assert_eq!(after[0]["text"], "Q");
        assert!(
            after[0].get("title").is_none(),
            "no field bleeds from the previous block type"
        );
    }

    #[test]
    fn set_and_find_block_rows_with_locale() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT,
                _locale TEXT
            );",
        );

        let blocks_en = vec![json!({"_block_type": "text", "body": "Hello"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_en, Some("en")).unwrap();

        let blocks_de = vec![json!({"_block_type": "text", "body": "Hallo"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_de, Some("de")).unwrap();

        let en = find_block_rows(&conn, "posts", "content", "p1", Some("en")).unwrap();
        assert_eq!(en.len(), 1);
        assert_eq!(en[0]["body"], "Hello");

        let de = find_block_rows(&conn, "posts", "content", "p1", Some("de")).unwrap();
        assert_eq!(de.len(), 1);
        assert_eq!(de[0]["body"], "Hallo");
    }

    #[test]
    fn set_block_rows_missing_block_type_errors() {
        let (_dir, conn) = setup_blocks_db();
        let blocks = vec![json!({"text": "no block type here"})];
        let result = set_block_rows(&conn, "posts", "content", "p1", &blocks, None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("missing '_block_type'"),
            "Error should mention missing _block_type, got: {msg}"
        );
    }

    #[test]
    fn set_block_rows_missing_block_type_errors_with_locale() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT,
                _locale TEXT
            );",
        );
        let blocks = vec![json!({"text": "no block type"})];
        let result = set_block_rows(&conn, "posts", "content", "p1", &blocks, Some("en"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("missing '_block_type'"),
            "Error should mention missing _block_type, got: {msg}"
        );
    }

    #[test]
    fn set_block_rows_empty_clears_locale() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT,
                _locale TEXT
            );",
        );

        let blocks = vec![json!({"_block_type": "text", "body": "Hi"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks, Some("en")).unwrap();

        // Clearing with empty slice should remove only the en locale rows
        set_block_rows(&conn, "posts", "content", "p1", &[], Some("en")).unwrap();
        let en = find_block_rows(&conn, "posts", "content", "p1", Some("en")).unwrap();
        assert!(en.is_empty());
    }
}
