//! Row-to-document conversion from SQLite result sets.

use rusqlite::Row;
use crate::core::Document;
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
                created_at = row.get::<_, Option<String>>(name.as_str())?;
            }
            "updated_at" => {
                updated_at = row.get::<_, Option<String>>(name.as_str())?;
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
