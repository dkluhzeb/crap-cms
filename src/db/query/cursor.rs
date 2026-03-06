//! Cursor-based (keyset) pagination support.
//!
//! Cursors are encoded as base64url(JSON). They contain the sort column,
//! direction, last sort value, and document ID (tiebreaker).

use anyhow::{Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::core::Document;

/// The engine used for cursor encoding — URL-safe base64 without padding.
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Data carried inside a cursor token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CursorData {
    pub sort_col: String,
    pub sort_dir: String,
    pub sort_val: serde_json::Value,
    pub id: String,
}

impl CursorData {
    /// Encode cursor data to a base64url string.
    pub fn encode(&self) -> Result<String> {
        let json = serde_json::to_string(self)?;
        Ok(B64.encode(json.as_bytes()))
    }

    /// Decode a base64url string into cursor data.
    pub fn decode(s: &str) -> Result<Self> {
        let bytes = B64.decode(s.as_bytes())
            .map_err(|e| anyhow::anyhow!("Invalid cursor encoding: {}", e))?;
        let json_str = std::str::from_utf8(&bytes)
            .map_err(|e| anyhow::anyhow!("Invalid cursor UTF-8: {}", e))?;
        let data: CursorData = serde_json::from_str(json_str)
            .map_err(|e| anyhow::anyhow!("Invalid cursor JSON: {}", e))?;
        if data.sort_col.is_empty() || data.id.is_empty() {
            bail!("Cursor missing required fields");
        }
        if data.sort_dir != "ASC" && data.sort_dir != "DESC" {
            bail!("Cursor sort_dir must be ASC or DESC");
        }
        Ok(data)
    }
}

/// Build start/end cursor strings from a page of documents.
///
/// - `start_cursor`: cursor of the **first** doc (always present if results non-empty).
/// - `end_cursor`: cursor of the **last** doc (always present if results non-empty).
///
/// These describe the current result set's boundaries. `has_next_page`/`has_prev_page`
/// are computed separately by the caller.
pub fn build_cursors(
    docs: &[Document],
    sort_col: &str,
    sort_dir: &str,
) -> (Option<String>, Option<String>) {
    if docs.is_empty() {
        return (None, None);
    }

    let start_cursor = cursor_from_doc(&docs[0], sort_col, sort_dir);
    let end_cursor = cursor_from_doc(&docs[docs.len() - 1], sort_col, sort_dir);

    (start_cursor, end_cursor)
}

/// Extract cursor data from a document.
fn cursor_from_doc(doc: &Document, sort_col: &str, sort_dir: &str) -> Option<String> {
    let sort_val = if sort_col == "id" {
        serde_json::Value::String(doc.id.clone())
    } else if sort_col == "created_at" {
        match &doc.created_at {
            Some(v) => serde_json::Value::String(v.clone()),
            None => serde_json::Value::Null,
        }
    } else if sort_col == "updated_at" {
        match &doc.updated_at {
            Some(v) => serde_json::Value::String(v.clone()),
            None => serde_json::Value::Null,
        }
    } else {
        doc.fields.get(sort_col).cloned().unwrap_or(serde_json::Value::Null)
    };

    let cursor = CursorData {
        sort_col: sort_col.to_string(),
        sort_dir: sort_dir.to_string(),
        sort_val,
        id: doc.id.clone(),
    };
    cursor.encode().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn encode_decode_roundtrip() {
        let cursor = CursorData {
            sort_col: "created_at".to_string(),
            sort_dir: "DESC".to_string(),
            sort_val: serde_json::json!("2024-06-01T12:00:00"),
            id: "abc123".to_string(),
        };
        let encoded = cursor.encode().unwrap();
        let decoded = CursorData::decode(&encoded).unwrap();
        assert_eq!(cursor, decoded);
    }

    #[test]
    fn decode_invalid_base64() {
        let result = CursorData::decode("not-valid-base64!!!");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid cursor"));
    }

    #[test]
    fn decode_invalid_json() {
        let encoded = B64.encode(b"not json");
        let result = CursorData::decode(&encoded);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid cursor JSON"));
    }

    #[test]
    fn decode_missing_fields() {
        let json = r#"{"sort_col":"","sort_dir":"ASC","sort_val":"x","id":"abc"}"#;
        let encoded = B64.encode(json.as_bytes());
        let result = CursorData::decode(&encoded);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing required fields"));
    }

    #[test]
    fn decode_invalid_sort_dir() {
        let json = r#"{"sort_col":"title","sort_dir":"UPDOWN","sort_val":"x","id":"abc"}"#;
        let encoded = B64.encode(json.as_bytes());
        let result = CursorData::decode(&encoded);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("sort_dir must be ASC or DESC"));
    }

    #[test]
    fn build_cursors_both_when_results_exist() {
        let docs: Vec<Document> = (0..3).map(|i| Document {
            id: format!("id{}", i),
            fields: HashMap::from([("title".to_string(), serde_json::json!(format!("Post {}", i)))]),
            created_at: Some(format!("2024-0{}-01", i + 1)),
            updated_at: None,
        }).collect();

        let (start, end) = build_cursors(&docs, "created_at", "ASC");
        assert!(start.is_some(), "start_cursor should exist when results non-empty");
        assert!(end.is_some(), "end_cursor should exist when results non-empty");

        // start_cursor points to first doc
        let decoded_start = CursorData::decode(&start.unwrap()).unwrap();
        assert_eq!(decoded_start.id, "id0");
        assert_eq!(decoded_start.sort_col, "created_at");

        // end_cursor points to last doc
        let decoded_end = CursorData::decode(&end.unwrap()).unwrap();
        assert_eq!(decoded_end.id, "id2");
        assert_eq!(decoded_end.sort_col, "created_at");
    }

    #[test]
    fn build_cursors_single_doc() {
        let docs = vec![Document {
            id: "only".to_string(),
            fields: HashMap::new(),
            created_at: Some("2024-01-01".to_string()),
            updated_at: None,
        }];

        let (start, end) = build_cursors(&docs, "created_at", "ASC");
        assert!(start.is_some());
        assert!(end.is_some());

        // Both point to the same doc
        let decoded_start = CursorData::decode(&start.unwrap()).unwrap();
        let decoded_end = CursorData::decode(&end.unwrap()).unwrap();
        assert_eq!(decoded_start.id, "only");
        assert_eq!(decoded_end.id, "only");
    }

    #[test]
    fn build_cursors_empty_docs() {
        let (start, end) = build_cursors(&[], "id", "ASC");
        assert!(start.is_none());
        assert!(end.is_none());
    }

    #[test]
    fn cursor_with_null_sort_val() {
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::Value::Null,
            id: "abc".to_string(),
        };
        let encoded = cursor.encode().unwrap();
        let decoded = CursorData::decode(&encoded).unwrap();
        assert_eq!(decoded.sort_val, serde_json::Value::Null);
    }
}
