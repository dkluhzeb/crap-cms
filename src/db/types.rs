//! Backend-agnostic database value and row types.

use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Number, Value};

/// Backend-agnostic database value — replaces `rusqlite::types::Value` and `Box<dyn ToSql>`.
#[derive(Debug, Clone, PartialEq)]
pub enum DbValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl DbValue {
    /// Returns `true` if this value is `Null`.
    pub fn is_null(&self) -> bool {
        matches!(self, DbValue::Null)
    }

    /// Convert to a JSON value.
    pub fn to_json(&self) -> Value {
        match self {
            DbValue::Null => Value::Null,
            DbValue::Integer(i) => Value::Number((*i).into()),
            DbValue::Real(f) => Number::from_f64(*f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            DbValue::Text(s) => Value::String(s.clone()),
            DbValue::Blob(b) => Value::String(STANDARD.encode(b)),
        }
    }
}

/// An owned database row — replaces the `rusqlite::Row` callback pattern.
#[derive(Debug, Clone)]
pub struct DbRow {
    pub(crate) columns: Vec<String>,
    pub(crate) values: Vec<DbValue>,
}

impl DbRow {
    /// Create a new row from columns and values.
    pub fn new(columns: Vec<String>, values: Vec<DbValue>) -> Self {
        Self { columns, values }
    }

    /// Get the column names.
    pub fn column_names(&self) -> &[String] {
        &self.columns
    }

    /// Get a value by column index.
    pub fn get_value(&self, idx: usize) -> Option<&DbValue> {
        self.values.get(idx)
    }

    /// Get a value by column name.
    pub fn get_named(&self, name: &str) -> Option<&DbValue> {
        self.columns
            .iter()
            .position(|c| c == name)
            .and_then(|idx| self.values.get(idx))
    }

    /// Get an i64 by column name.
    pub fn get_i64(&self, name: &str) -> Result<i64> {
        match self.get_named(name) {
            Some(DbValue::Integer(i)) => Ok(*i),
            Some(other) => bail!("column '{}': expected Integer, got {:?}", name, other),
            None => bail!("column '{}' not found", name),
        }
    }

    /// Get an f64 by column name.
    pub fn get_f64(&self, name: &str) -> Result<f64> {
        match self.get_named(name) {
            Some(DbValue::Real(f)) => Ok(*f),
            Some(DbValue::Integer(i)) => Ok(*i as f64),
            Some(other) => bail!("column '{}': expected Real, got {:?}", name, other),
            None => bail!("column '{}' not found", name),
        }
    }

    /// Get a string by column name. Returns an error if the column is missing or not text.
    pub fn get_string(&self, name: &str) -> Result<String> {
        match self.get_named(name) {
            Some(DbValue::Text(s)) => Ok(s.clone()),
            Some(DbValue::Null) => bail!("column '{}': value is NULL", name),
            Some(other) => bail!("column '{}': expected Text, got {:?}", name, other),
            None => bail!("column '{}' not found", name),
        }
    }

    /// Get an optional string by column name. Returns `None` for NULL values.
    pub fn get_opt_string(&self, name: &str) -> Result<Option<String>> {
        match self.get_named(name) {
            Some(DbValue::Text(s)) => Ok(Some(s.clone())),
            Some(DbValue::Null) => Ok(None),
            Some(other) => bail!("column '{}': expected Text or Null, got {:?}", name, other),
            None => bail!("column '{}' not found", name),
        }
    }

    /// Get a boolean by column name (INTEGER: 0 = false, nonzero = true).
    pub fn get_bool(&self, name: &str) -> Result<bool> {
        match self.get_named(name) {
            Some(DbValue::Integer(i)) => Ok(*i != 0),
            Some(DbValue::Null) => Ok(false),
            Some(other) => bail!(
                "column '{}': expected Integer (bool), got {:?}",
                name,
                other
            ),
            None => bail!("column '{}' not found", name),
        }
    }

    /// Get the JSON value for a column by name, converting from the underlying DbValue.
    pub fn get_json(&self, name: &str) -> Result<Value> {
        match self.get_named(name) {
            Some(v) => Ok(v.to_json()),
            None => bail!("column '{}' not found", name),
        }
    }

    /// Number of columns in this row.
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_row() -> DbRow {
        DbRow::new(
            vec![
                "id".into(),
                "name".into(),
                "age".into(),
                "score".into(),
                "bio".into(),
                "active".into(),
            ],
            vec![
                DbValue::Text("doc1".into()),
                DbValue::Text("Alice".into()),
                DbValue::Integer(30),
                DbValue::Real(95.5),
                DbValue::Null,
                DbValue::Integer(1),
            ],
        )
    }

    #[test]
    fn get_string_returns_text() {
        let row = sample_row();
        assert_eq!(row.get_string("name").unwrap(), "Alice");
    }

    #[test]
    fn get_string_null_is_error() {
        let row = sample_row();
        assert!(row.get_string("bio").is_err());
    }

    #[test]
    fn get_opt_string_returns_none_for_null() {
        let row = sample_row();
        assert_eq!(row.get_opt_string("bio").unwrap(), None);
    }

    #[test]
    fn get_opt_string_returns_some() {
        let row = sample_row();
        assert_eq!(row.get_opt_string("name").unwrap(), Some("Alice".into()));
    }

    #[test]
    fn get_i64_returns_integer() {
        let row = sample_row();
        assert_eq!(row.get_i64("age").unwrap(), 30);
    }

    #[test]
    fn get_f64_returns_real() {
        let row = sample_row();
        assert!((row.get_f64("score").unwrap() - 95.5).abs() < f64::EPSILON);
    }

    #[test]
    fn get_f64_coerces_integer() {
        let row = sample_row();
        assert!((row.get_f64("age").unwrap() - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn get_bool_returns_true_for_nonzero() {
        let row = sample_row();
        assert!(row.get_bool("active").unwrap());
    }

    #[test]
    fn get_bool_returns_false_for_null() {
        let row = sample_row();
        assert!(!row.get_bool("bio").unwrap());
    }

    #[test]
    fn get_named_missing_column() {
        let row = sample_row();
        assert!(row.get_named("nonexistent").is_none());
    }

    #[test]
    fn get_json_converts_values() {
        let row = sample_row();
        assert_eq!(row.get_json("name").unwrap(), Value::String("Alice".into()));
        assert_eq!(row.get_json("age").unwrap(), serde_json::json!(30));
        assert!(row.get_json("bio").unwrap().is_null());
    }

    #[test]
    fn column_count_and_names() {
        let row = sample_row();
        assert_eq!(row.column_count(), 6);
        assert_eq!(row.column_names()[0], "id");
    }

    #[test]
    fn dbvalue_is_null() {
        assert!(DbValue::Null.is_null());
        assert!(!DbValue::Integer(0).is_null());
    }

    #[test]
    fn dbvalue_to_json() {
        assert_eq!(DbValue::Null.to_json(), Value::Null);
        assert_eq!(DbValue::Integer(42).to_json(), serde_json::json!(42));
        assert_eq!(DbValue::Real(3.14).to_json(), serde_json::json!(3.14));
        assert_eq!(
            DbValue::Text("hi".into()).to_json(),
            Value::String("hi".into())
        );
        assert_eq!(
            DbValue::Blob(vec![1, 2]).to_json(),
            Value::String("AQI=".into())
        );
    }
}
