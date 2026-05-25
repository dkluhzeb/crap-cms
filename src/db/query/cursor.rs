//! Cursor-based (keyset) pagination support.
//!
//! Cursors are encoded as base64url(JSON). They contain the sort column,
//! direction, last sort value, and document ID (tiebreaker).

use std::{fmt, str, str::FromStr};

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{core::Document, db::DbValue};

/// Sort direction for ORDER BY clauses and cursor pagination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SortDirection {
    #[default]
    #[serde(rename = "ASC")]
    Asc,
    #[serde(rename = "DESC")]
    Desc,
}

impl SortDirection {
    /// SQL keyword for this direction.
    #[must_use]
    pub fn as_sql(&self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }

    /// Return the opposite direction.
    #[must_use]
    pub fn flip(&self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }
}

impl fmt::Display for SortDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_sql())
    }
}

impl FromStr for SortDirection {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "ASC" => Ok(Self::Asc),
            "DESC" => Ok(Self::Desc),
            other => bail!("Invalid sort direction '{other}': must be ASC or DESC"),
        }
    }
}

/// The engine used for cursor encoding — URL-safe base64 without padding.
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Typed cursor sort value. Mirrors the sortable subset of [`DbValue`] —
/// `Blob` is excluded because columns of that kind are never sorted on.
///
/// Serializes untagged so the wire format matches what previous releases
/// emitted (raw JSON scalar in the `sort_val` slot of the cursor JSON);
/// existing pre-typed cursor URLs decode without churn.
///
/// Variant order matters for `untagged` deserialization — `Bool` is tried
/// before `Integer` so JSON `true`/`false` doesn't get accidentally
/// coerced; `Integer` before `Real` so `42` round-trips to `Integer(42)`
/// and only fractional numbers fall through to `Real`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SortValue {
    #[default]
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    Text(String),
}

impl From<&Value> for SortValue {
    fn from(v: &Value) -> Self {
        match v {
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Bool(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Self::Integer(i)
                } else if let Some(f) = n.as_f64() {
                    Self::Real(f)
                } else {
                    // Out-of-range / NaN — degrade to text to preserve
                    // legacy behavior (the previous Value path stringified).
                    Self::Text(n.to_string())
                }
            }
            Value::String(s) => Self::Text(s.clone()),
            // Arrays/objects can't be sorted on at the SQL layer; the
            // previous Value path stringified them via `Value::to_string`.
            other => Self::Text(other.to_string()),
        }
    }
}

impl From<&SortValue> for DbValue {
    fn from(v: &SortValue) -> Self {
        match v {
            SortValue::Null => DbValue::Null,
            // Booleans land in SQLite as INTEGER 0/1 — same coercion the
            // ORM applies for boolean column writes.
            SortValue::Bool(b) => DbValue::Integer(i64::from(*b)),
            SortValue::Integer(i) => DbValue::Integer(*i),
            SortValue::Real(f) => DbValue::Real(*f),
            SortValue::Text(s) => DbValue::Text(s.clone()),
        }
    }
}

/// Predicate gating the composite `_status`-aware cursor ordering.
///
/// When this returns true the `find` SQL prepends `_status DIR` to the
/// ORDER BY (so drafts surface above published regardless of the
/// configured `default_sort`), and the cursor encoder records each
/// row's `_status` in `CursorData::status_val` so the keyset can
/// compare against the composite `(_status, sort_col, id)` order.
///
/// Both the SQL writer (`apply_order_by`) and the cursor builder
/// (`PaginationResult::cursor` → `build_cursors`) must call this — if
/// the two sides disagree the keyset references a column the ORDER BY
/// doesn't tiebreak on, and prev/next stops being symmetric. Sharing
/// one predicate keeps them locked together.
pub(crate) fn cursor_status_active(has_drafts: bool, sort_col: &str) -> bool {
    has_drafts && sort_col != "_status"
}

/// Data carried inside a cursor token.
///
/// `status_val` is populated only for collections with drafts, where
/// `apply_order_by` prepends `_status ASC` to surface drafts above
/// published. The keyset comparison then runs against the composite
/// `(_status, sort_col, id)` order so prev/next stay symmetric across
/// the draft↔published boundary. `#[serde(default)]` keeps decoding
/// of pre-composite cursor URLs working — they just decode with
/// `status_val = None` and fall back to single-column keyset.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CursorData {
    pub sort_col: String,
    pub sort_dir: SortDirection,
    pub sort_val: SortValue,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_val: Option<String>,
}

impl CursorData {
    /// Encode cursor data to a base64url string.
    ///
    /// # Errors
    ///
    /// Returns an error if JSON serialization fails (should not happen with
    /// the well-formed `CursorData` shape).
    pub fn encode(&self) -> Result<String> {
        let json = serde_json::to_string(self)?;

        Ok(B64.encode(json.as_bytes()))
    }

    /// Decode a base64url string into cursor data.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is not valid base64url, not valid
    /// UTF-8 JSON, or missing required fields (`sort_col`, `id`).
    pub fn decode(s: &str) -> Result<Self> {
        let bytes = B64
            .decode(s.as_bytes())
            .map_err(|e| anyhow!("Invalid cursor encoding: {e}"))?;
        let json_str = str::from_utf8(&bytes).map_err(|e| anyhow!("Invalid cursor UTF-8: {e}"))?;
        let data: CursorData =
            serde_json::from_str(json_str).map_err(|e| anyhow!("Invalid cursor JSON: {e}"))?;

        if data.sort_col.is_empty() || data.id.is_empty() {
            bail!("Cursor missing required fields");
        }

        Ok(data)
    }
}

/// Build start/end cursor strings from a page of documents.
///
/// - `start_cursor`: cursor of the **first** doc (always present if results non-empty).
/// - `end_cursor`: cursor of the **last** doc (always present if results non-empty).
///
/// `with_status` records whether `apply_order_by` is prepending
/// `_status ASC` to the ORDER BY (true on collections with drafts when
/// `sort_col != "_status"`). When true, each cursor also encodes the
/// document's `_status` so the keyset can compare against the
/// composite `(_status, sort_col, id)` order. The caller (`find_documents`
/// / `find_globals`) passes this consistently with what
/// `apply_order_by` does for the same query.
pub(crate) fn build_cursors(
    docs: &[Document],
    sort_col: &str,
    sort_dir: SortDirection,
    with_status: bool,
) -> (Option<String>, Option<String>) {
    if docs.is_empty() {
        return (None, None);
    }

    let start_cursor = cursor_from_doc(&docs[0], sort_col, sort_dir, with_status);
    let end_cursor = cursor_from_doc(docs.last().unwrap(), sort_col, sort_dir, with_status);

    (start_cursor, end_cursor)
}

/// Extract cursor data from a document.
fn cursor_from_doc(
    doc: &Document,
    sort_col: &str,
    sort_dir: SortDirection,
    with_status: bool,
) -> Option<String> {
    let sort_val = match sort_col {
        "id" => SortValue::Text(doc.id.to_string()),
        "created_at" => doc
            .created_at
            .as_ref()
            .map(|v| SortValue::Text(v.clone()))
            .unwrap_or_default(),
        "updated_at" => doc
            .updated_at
            .as_ref()
            .map(|v| SortValue::Text(v.clone()))
            .unwrap_or_default(),
        col => doc.fields.get(col).map(SortValue::from).unwrap_or_default(),
    };

    let status_val = if with_status {
        doc.fields
            .get("_status")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            // Fall back to the column default — the row exists in a
            // drafts-enabled collection, so `_status` should be set,
            // but the SELECT might have stripped it under select={}.
            .or_else(|| Some("published".to_string()))
    } else {
        None
    };

    let cursor = CursorData {
        sort_col: sort_col.to_string(),
        sort_dir,
        sort_val,
        id: doc.id.to_string(),
        status_val,
    };

    match cursor.encode() {
        Ok(encoded) => Some(encoded),
        Err(e) => {
            tracing::error!(
                "Failed to encode pagination cursor for doc {}: {:#}",
                doc.id,
                e
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let cursor = CursorData {
            sort_col: "created_at".to_string(),
            sort_dir: SortDirection::Desc,
            sort_val: SortValue::Text("2024-06-01T12:00:00".to_string()),
            id: "abc123".to_string(),
            ..Default::default()
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid cursor JSON")
        );
    }

    #[test]
    fn decode_missing_fields() {
        let json = r#"{"sort_col":"","sort_dir":"ASC","sort_val":"x","id":"abc"}"#;
        let encoded = B64.encode(json.as_bytes());
        let result = CursorData::decode(&encoded);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing required fields")
        );
    }

    #[test]
    fn decode_invalid_sort_dir() {
        let json = r#"{"sort_col":"title","sort_dir":"UPDOWN","sort_val":"x","id":"abc"}"#;
        let encoded = B64.encode(json.as_bytes());
        let result = CursorData::decode(&encoded);
        assert!(
            result.is_err(),
            "Invalid sort_dir should fail deserialization"
        );
    }

    #[test]
    fn build_cursors_both_when_results_exist() {
        let docs: Vec<Document> = (0..3)
            .map(|i| {
                let mut d = Document::new(format!("id{i}"));
                d.fields
                    .insert("title".to_string(), json!(format!("Post {}", i)));
                d.created_at = Some(format!("2024-0{}-01", i + 1));
                d
            })
            .collect();

        let (start, end) = build_cursors(&docs, "created_at", SortDirection::Asc, false);
        assert!(
            start.is_some(),
            "start_cursor should exist when results non-empty"
        );
        assert!(
            end.is_some(),
            "end_cursor should exist when results non-empty"
        );

        let decoded_start = CursorData::decode(&start.unwrap()).unwrap();
        assert_eq!(decoded_start.id, "id0");
        assert_eq!(decoded_start.sort_col, "created_at");

        let decoded_end = CursorData::decode(&end.unwrap()).unwrap();
        assert_eq!(decoded_end.id, "id2");
        assert_eq!(decoded_end.sort_col, "created_at");
    }

    #[test]
    fn build_cursors_single_doc() {
        let mut doc = Document::new("only".to_string());
        doc.created_at = Some("2024-01-01".to_string());
        let docs = vec![doc];

        let (start, end) = build_cursors(&docs, "created_at", SortDirection::Asc, false);
        assert!(start.is_some());
        assert!(end.is_some());

        let decoded_start = CursorData::decode(&start.unwrap()).unwrap();
        let decoded_end = CursorData::decode(&end.unwrap()).unwrap();
        assert_eq!(decoded_start.id, "only");
        assert_eq!(decoded_end.id, "only");
    }

    #[test]
    fn build_cursors_empty_docs() {
        let (start, end) = build_cursors(&[], "id", SortDirection::Asc, false);
        assert!(start.is_none());
        assert!(end.is_none());
    }

    #[test]
    fn cursor_with_null_sort_val() {
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: SortValue::Null,
            id: "abc".to_string(),
            ..Default::default()
        };
        let encoded = cursor.encode().unwrap();
        let decoded = CursorData::decode(&encoded).unwrap();
        assert_eq!(decoded.sort_val, SortValue::Null);
    }

    #[test]
    fn decode_empty_id_field_errors() {
        let json = r#"{"sort_col":"title","sort_dir":"ASC","sort_val":"x","id":""}"#;
        let encoded = B64.encode(json.as_bytes());
        let result = CursorData::decode(&encoded);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing required fields")
        );
    }

    #[test]
    fn decode_invalid_utf8() {
        let bad_bytes: &[u8] = &[0xFF, 0xFE, 0xFD];
        let encoded = B64.encode(bad_bytes);
        let result = CursorData::decode(&encoded);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid cursor UTF-8")
        );
    }

    #[test]
    fn build_cursors_sort_by_updated_at_with_value() {
        let mut doc = Document::new("doc1".to_string());
        doc.updated_at = Some("2024-06-15".to_string());
        let docs = vec![doc];
        let (start, end) = build_cursors(&docs, "updated_at", SortDirection::Desc, false);
        let decoded_start = CursorData::decode(&start.unwrap()).unwrap();
        let decoded_end = CursorData::decode(&end.unwrap()).unwrap();
        assert_eq!(decoded_start.sort_col, "updated_at");
        assert_eq!(
            decoded_start.sort_val,
            SortValue::Text("2024-06-15".to_string())
        );
        assert_eq!(decoded_end.sort_col, "updated_at");
    }

    #[test]
    fn build_cursors_sort_by_updated_at_none() {
        let docs = vec![Document::new("doc1".to_string())];
        let (start, _end) = build_cursors(&docs, "updated_at", SortDirection::Asc, false);
        let decoded = CursorData::decode(&start.unwrap()).unwrap();
        assert_eq!(decoded.sort_val, SortValue::Null);
    }

    #[test]
    fn build_cursors_sort_by_created_at_none() {
        let docs = vec![Document::new("doc2".to_string())];
        let (start, _end) = build_cursors(&docs, "created_at", SortDirection::Asc, false);
        let decoded = CursorData::decode(&start.unwrap()).unwrap();
        assert_eq!(decoded.sort_val, SortValue::Null);
    }

    #[test]
    fn build_cursors_sort_by_id() {
        let docs = vec![Document::new("the-id".to_string())];
        let (start, _end) = build_cursors(&docs, "id", SortDirection::Asc, false);
        let decoded = CursorData::decode(&start.unwrap()).unwrap();
        assert_eq!(decoded.sort_col, "id");
        assert_eq!(decoded.sort_val, SortValue::Text("the-id".to_string()));
    }

    #[test]
    fn build_cursors_sort_by_arbitrary_field_present() {
        let mut doc = Document::new("doc3".to_string());
        doc.fields.insert("score".to_string(), json!(42));
        let docs = vec![doc];
        let (start, _end) = build_cursors(&docs, "score", SortDirection::Asc, false);
        let decoded = CursorData::decode(&start.unwrap()).unwrap();
        assert_eq!(decoded.sort_col, "score");
        assert_eq!(decoded.sort_val, SortValue::Integer(42));
    }

    #[test]
    fn build_cursors_sort_by_arbitrary_field_missing() {
        let docs = vec![Document::new("doc4".to_string())];
        let (start, _end) = build_cursors(&docs, "nonexistent", SortDirection::Asc, false);
        let decoded = CursorData::decode(&start.unwrap()).unwrap();
        assert_eq!(decoded.sort_val, SortValue::Null);
    }

    #[test]
    fn from_value_picks_integer_over_real_for_whole_numbers() {
        assert_eq!(SortValue::from(&json!(42)), SortValue::Integer(42));
        assert_eq!(SortValue::from(&json!(0.5)), SortValue::Real(0.5));
    }

    #[test]
    fn from_value_falls_through_to_text_for_arrays_objects() {
        let arr = SortValue::from(&json!(["a", "b"]));
        assert!(matches!(arr, SortValue::Text(_)));
        let obj = SortValue::from(&json!({"k": "v"}));
        assert!(matches!(obj, SortValue::Text(_)));
    }

    #[test]
    fn db_value_conversion_matches_legacy() {
        assert_eq!(DbValue::from(&SortValue::Null), DbValue::Null);
        assert_eq!(DbValue::from(&SortValue::Integer(7)), DbValue::Integer(7));
        assert_eq!(DbValue::from(&SortValue::Real(1.5)), DbValue::Real(1.5));
        assert_eq!(
            DbValue::from(&SortValue::Text("abc".into())),
            DbValue::Text("abc".into())
        );
        assert_eq!(DbValue::from(&SortValue::Bool(true)), DbValue::Integer(1));
        assert_eq!(DbValue::from(&SortValue::Bool(false)), DbValue::Integer(0));
    }
}
