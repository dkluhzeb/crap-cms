//! Long-lived account credentials an export can carry to another installation.

use std::collections::HashMap;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value};

use crate::db::{DbConnection, DbValue};

/// The stored columns that make up an account's credentials: its password,
/// lock, session version, settings, verification and TOTP state. One-time
/// tokens (password reset, email verification, MFA codes) are never carried.
pub const CREDENTIAL_COLUMNS: &[&str] = &[
    "_password_hash",
    "_locked",
    "_session_version",
    "_settings",
    "_verified",
    "_totp_secret",
    "_totp_confirmed",
    "_totp_last_step",
];

/// The credential columns `table` has — which ones depends on the collection's
/// auth settings (verification, TOTP).
///
/// # Errors
///
/// Returns a backend error if the table's columns can't be read.
pub fn credential_columns(conn: &dyn DbConnection, table: &str) -> Result<Vec<&'static str>> {
    let existing = conn.get_table_column_types(table)?;

    Ok(CREDENTIAL_COLUMNS
        .iter()
        .copied()
        .filter(|col| existing.contains_key(*col))
        .collect())
}

/// Read the credentials of every account in `slug`, trashed ones included,
/// keyed by document id.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails or a row fails to parse.
pub fn read_credentials(
    conn: &dyn DbConnection,
    slug: &str,
) -> Result<HashMap<String, Map<String, Value>>> {
    let columns = credential_columns(conn, slug)?;
    if columns.is_empty() {
        return Ok(HashMap::new());
    }

    let sql = format!("SELECT id, {} FROM \"{slug}\"", columns.join(", "));
    let rows = conn
        .query_all(&sql, &[])
        .with_context(|| format!("Failed to read credentials from {slug}"))?;

    rows.iter()
        .map(|row| {
            let values = columns
                .iter()
                .enumerate()
                .map(|(i, col)| {
                    let value = row.get_value(i + 1).map_or(Value::Null, DbValue::to_json);
                    ((*col).to_string(), value)
                })
                .collect();

            Ok((row.get_string("id")?, values))
        })
        .collect()
}

/// The column values to write for an imported account's credentials, keeping
/// only the columns `available` lists (state the target collection doesn't
/// use, such as TOTP on a collection without it, is skipped).
///
/// # Errors
///
/// Returns an error naming a key that isn't a credential, or a value that is
/// neither a string, an integer, a boolean nor null.
pub fn credential_values(
    credentials: &Map<String, Value>,
    available: &[&str],
) -> Result<Vec<(String, DbValue)>> {
    let mut values = Vec::new();

    for (key, value) in credentials {
        if !CREDENTIAL_COLUMNS.contains(&key.as_str()) {
            bail!("'{key}' is not an account credential");
        }

        if available.contains(&key.as_str()) {
            values.push((key.clone(), credential_value(key, value)?));
        }
    }

    Ok(values)
}

fn credential_value(key: &str, value: &Value) -> Result<DbValue> {
    match value {
        Value::Null => Ok(DbValue::Null),
        Value::String(s) => Ok(DbValue::Text(s.clone())),
        Value::Bool(b) => Ok(DbValue::Integer(i64::from(*b))),
        Value::Number(n) => n
            .as_i64()
            .map(DbValue::Integer)
            .ok_or_else(|| anyhow!("credential '{key}' must be an integer, got {n}")),
        other => bail!("credential '{key}' must be a string or an integer, got {other}"),
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::db::InMemoryConn;

    fn users() -> InMemoryConn {
        let conn = InMemoryConn::open();
        conn.setup(
            "CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT, _password_hash TEXT,
                 _reset_token TEXT, _locked INTEGER, _session_version INTEGER, _settings TEXT);
             INSERT INTO users VALUES ('u1', 'a@example.com', '$argon2id$hash', 'reset', 1, 3, NULL);",
        );
        conn
    }

    /// Only the credential columns the table has are read; one-time tokens
    /// never are.
    #[test]
    fn reads_present_credentials_without_tokens() {
        let conn = users();

        let credentials = read_credentials(&conn, "users").unwrap();

        assert_eq!(
            Value::Object(credentials["u1"].clone()),
            json!({
                "_password_hash": "$argon2id$hash",
                "_locked": 1,
                "_session_version": 3,
                "_settings": null,
            })
        );
    }

    #[test]
    fn values_skip_unused_columns_and_reject_other_keys() {
        let available = ["_password_hash", "_locked"];

        let mut values = credential_values(
            json!({ "_password_hash": "h", "_locked": true, "_totp_secret": "s" })
                .as_object()
                .unwrap(),
            &available,
        )
        .unwrap();
        values.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            values,
            vec![
                ("_locked".to_string(), DbValue::Integer(1)),
                ("_password_hash".to_string(), DbValue::Text("h".into())),
            ]
        );

        let err = credential_values(
            json!({ "_reset_token": "t" }).as_object().unwrap(),
            &available,
        )
        .unwrap_err();
        assert!(err.to_string().contains("_reset_token"), "{err}");
    }
}
