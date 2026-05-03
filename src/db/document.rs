//! Row-to-document conversion from database result sets.

use anyhow::Result;
use serde_json::Value;

use crate::core::Document;
use std::collections::HashMap;

use super::{
    connection::DbConnection,
    types::{DbRow, DbValue},
};

/// Convert a `DbRow` to a `Document`.
pub fn row_to_document(conn: &dyn DbConnection, row: &DbRow) -> Result<Document> {
    let id = row.get_string("id")?;
    let mut fields = HashMap::new();
    let mut created_at = None;
    let mut updated_at = None;

    for (i, name) in row.column_names().iter().enumerate() {
        match name.as_str() {
            "id" => continue,
            "created_at" => {
                if let Some(DbValue::Text(s)) = row.get_value(i) {
                    created_at = Some(conn.normalize_timestamp(s));
                }
            }
            "updated_at" => {
                if let Some(DbValue::Text(s)) = row.get_value(i) {
                    updated_at = Some(conn.normalize_timestamp(s));
                }
            }
            _ => {
                let value = row.get_value(i).map(|v| v.to_json()).unwrap_or(Value::Null);
                fields.insert(name.clone(), value);
            }
        }
    }

    Ok(Document::builder(id)
        .fields(fields)
        .created_at(created_at)
        .updated_at(updated_at)
        .build())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::db::{
        InMemoryConn,
        types::{DbRow, DbValue},
    };

    fn make_row(columns: Vec<&str>, values: Vec<DbValue>) -> DbRow {
        DbRow::new(columns.into_iter().map(|s| s.to_string()).collect(), values)
    }

    #[test]
    fn dbvalue_to_json_integer() {
        assert_eq!(DbValue::Integer(42).to_json(), json!(42));
    }

    #[test]
    fn dbvalue_to_json_float() {
        let val = DbValue::Real(3.15).to_json();
        assert!(val.as_f64().unwrap() > 3.1 && val.as_f64().unwrap() < 3.2);
    }

    #[test]
    fn dbvalue_to_json_text() {
        assert_eq!(DbValue::Text("hello".into()).to_json(), json!("hello"));
    }

    #[test]
    fn dbvalue_to_json_null() {
        assert!(DbValue::Null.to_json().is_null());
    }

    #[test]
    fn row_to_document_basic() {
        let conn = InMemoryConn::open();
        let row = make_row(
            vec!["id", "text_col", "int_col", "created_at", "updated_at"],
            vec![
                DbValue::Text("doc1".into()),
                DbValue::Text("hello".into()),
                DbValue::Integer(42),
                DbValue::Text("2024-01-01".into()),
                DbValue::Text("2024-01-02".into()),
            ],
        );
        let doc = row_to_document(&conn, &row).unwrap();
        assert_eq!(doc.id, "doc1");
        assert_eq!(doc.fields.get("text_col").unwrap(), &json!("hello"));
        assert_eq!(doc.fields.get("int_col").unwrap(), &json!(42));
        assert_eq!(doc.created_at.as_deref(), Some("2024-01-01"));
        assert_eq!(doc.updated_at.as_deref(), Some("2024-01-02"));
        // id, created_at, updated_at should NOT be in fields
        assert!(!doc.fields.contains_key("id"));
        assert!(!doc.fields.contains_key("created_at"));
        assert!(!doc.fields.contains_key("updated_at"));
    }

    #[test]
    fn normalize_legacy_timestamp() {
        let conn = InMemoryConn::open();
        assert_eq!(
            conn.normalize_timestamp("2024-01-01 12:00:00"),
            "2024-01-01T12:00:00.000Z"
        );
    }

    #[test]
    fn normalize_already_iso() {
        let conn = InMemoryConn::open();
        assert_eq!(
            conn.normalize_timestamp("2024-01-01T12:00:00.000Z"),
            "2024-01-01T12:00:00.000Z"
        );
    }
}
