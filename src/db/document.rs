//! Row-to-document conversion from SQLite result sets.

use crate::core::Document;
use rusqlite::Row;
use std::collections::HashMap;

/// Convert a rusqlite Row to a Document given the column names.
pub fn row_to_document(row: &Row, column_names: &[String]) -> rusqlite::Result<Document> {
    let id: String = row.get("id")?;
    let mut fields = HashMap::new();
    let mut created_at = None;
    let mut updated_at = None;

    for name in column_names {
        match name.as_str() {
            "id" => continue,
            "created_at" => {
                created_at = row
                    .get::<_, Option<String>>(name.as_str())?
                    .map(normalize_timestamp);
            }
            "updated_at" => {
                updated_at = row
                    .get::<_, Option<String>>(name.as_str())?
                    .map(normalize_timestamp);
            }
            _ => {
                let value = sqlite_value_to_json(row, name)?;
                fields.insert(name.clone(), value);
            }
        }
    }

    Ok(Document {
        id,
        fields,
        created_at,
        updated_at,
    })
}

/// Normalize legacy "YYYY-MM-DD HH:MM:SS" timestamps to ISO 8601 "YYYY-MM-DDTHH:MM:SS.000Z".
/// Already-normalized timestamps pass through unchanged.
fn normalize_timestamp(ts: String) -> String {
    if ts.len() == 19 && ts.as_bytes().get(10) == Some(&b' ') {
        format!("{}T{}.000Z", &ts[..10], &ts[11..])
    } else {
        ts
    }
}

/// Convert a SQLite column value to a JSON value.
fn sqlite_value_to_json(row: &Row, column: &str) -> rusqlite::Result<serde_json::Value> {
    // Try each type in order: integer, real, text, null
    if let Ok(v) = row.get::<_, i64>(column) {
        return Ok(serde_json::Value::Number(v.into()));
    }
    if let Ok(v) = row.get::<_, f64>(column) {
        if let Some(n) = serde_json::Number::from_f64(v) {
            return Ok(serde_json::Value::Number(n));
        }
    }
    if let Ok(v) = row.get::<_, String>(column) {
        return Ok(serde_json::Value::String(v));
    }
    Ok(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (
                id TEXT PRIMARY KEY,
                int_col INTEGER,
                real_col REAL,
                text_col TEXT,
                null_col TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();
        conn
    }

    #[test]
    fn sqlite_value_to_json_integer() {
        let conn = setup_test_db();
        conn.execute("INSERT INTO test (id, int_col) VALUES ('1', 42)", [])
            .unwrap();
        let mut stmt = conn
            .prepare("SELECT int_col FROM test WHERE id='1'")
            .unwrap();
        let val: serde_json::Value = stmt
            .query_row([], |row| sqlite_value_to_json(row, "int_col"))
            .unwrap();
        assert_eq!(val, serde_json::json!(42));
    }

    #[test]
    fn sqlite_value_to_json_float() {
        let conn = setup_test_db();
        conn.execute("INSERT INTO test (id, real_col) VALUES ('1', 3.14)", [])
            .unwrap();
        let mut stmt = conn
            .prepare("SELECT real_col FROM test WHERE id='1'")
            .unwrap();
        let val: serde_json::Value = stmt
            .query_row([], |row| sqlite_value_to_json(row, "real_col"))
            .unwrap();
        // SQLite stores 3.14 as float; check it's close
        assert!(val.as_f64().unwrap() > 3.13 && val.as_f64().unwrap() < 3.15);
    }

    #[test]
    fn sqlite_value_to_json_text() {
        let conn = setup_test_db();
        conn.execute("INSERT INTO test (id, text_col) VALUES ('1', 'hello')", [])
            .unwrap();
        let mut stmt = conn
            .prepare("SELECT text_col FROM test WHERE id='1'")
            .unwrap();
        let val: serde_json::Value = stmt
            .query_row([], |row| sqlite_value_to_json(row, "text_col"))
            .unwrap();
        assert_eq!(val, serde_json::json!("hello"));
    }

    #[test]
    fn sqlite_value_to_json_null() {
        let conn = setup_test_db();
        conn.execute("INSERT INTO test (id, null_col) VALUES ('1', NULL)", [])
            .unwrap();
        let mut stmt = conn
            .prepare("SELECT null_col FROM test WHERE id='1'")
            .unwrap();
        let val: serde_json::Value = stmt
            .query_row([], |row| sqlite_value_to_json(row, "null_col"))
            .unwrap();
        assert!(val.is_null());
    }

    #[test]
    fn row_to_document_basic() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO test (id, text_col, int_col, created_at, updated_at) VALUES ('doc1', 'hello', 42, '2024-01-01', '2024-01-02')",
            [],
        ).unwrap();
        let columns = vec![
            "id".to_string(),
            "text_col".to_string(),
            "int_col".to_string(),
            "created_at".to_string(),
            "updated_at".to_string(),
        ];
        let mut stmt = conn
            .prepare(
                "SELECT id, text_col, int_col, created_at, updated_at FROM test WHERE id='doc1'",
            )
            .unwrap();
        let doc = stmt
            .query_row([], |row| row_to_document(row, &columns))
            .unwrap();
        assert_eq!(doc.id, "doc1");
        assert_eq!(
            doc.fields.get("text_col").unwrap(),
            &serde_json::json!("hello")
        );
        assert_eq!(doc.fields.get("int_col").unwrap(), &serde_json::json!(42));
        assert_eq!(doc.created_at.as_deref(), Some("2024-01-01"));
        assert_eq!(doc.updated_at.as_deref(), Some("2024-01-02"));
        // id, created_at, updated_at should NOT be in fields
        assert!(!doc.fields.contains_key("id"));
        assert!(!doc.fields.contains_key("created_at"));
        assert!(!doc.fields.contains_key("updated_at"));
    }
}
