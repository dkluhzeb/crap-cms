//! Array field join table operations.

use anyhow::Result;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

use crate::core::{FieldDefinition, FieldType, field::flatten_array_sub_fields};
use crate::db::{
    DbConnection, DbRow, DbValue,
    query::{
        coerce_json_value,
        helpers::{coerce_date_value_json, join_table, tz_column},
    },
    types::real_to_json_number,
};

use super::helpers::{
    JunctionTarget, delete_junction_rows_except, existing_junction_ids, select_junction_rows,
    select_junction_rows_batch,
};

/// Coerce one flattened array sub-field to its DB value (and, for a
/// timezone-enabled Date, its `_tz` companion value). One place for the
/// per-column conversion so the INSERT and column-preserving UPDATE agree.
fn coerce_array_field(
    sf: &FieldDefinition,
    row: &HashMap<String, Value>,
) -> (DbValue, Option<DbValue>) {
    let value = row.get(&sf.name).cloned().unwrap_or(Value::Null);

    if sf.field_type == FieldType::Date && sf.timezone {
        let tz_key = tz_column(&sf.name);
        let db_val = coerce_date_value_json(
            &sf.field_type,
            &value,
            row.get(&tz_key).and_then(Value::as_str),
        );
        let tz_val = match row.get(&tz_key).and_then(Value::as_str) {
            Some(s) if !s.is_empty() => DbValue::Text(s.to_string()),
            _ => DbValue::Null,
        };
        return (db_val, Some(tz_val));
    }

    (coerce_json_value(&sf.field_type, &value), None)
}

/// INSERT a brand-new array row with every column (an absent sub-field is
/// written as null — a new row has no prior value to preserve).
fn insert_array_row(
    conn: &dyn DbConnection,
    target: &JunctionTarget,
    id: &str,
    order: i64,
    row: &HashMap<String, Value>,
    flat_subs: &[&FieldDefinition],
) -> Result<()> {
    let mut cols: Vec<String> = vec!["id".into(), "parent_id".into(), "_order".into()];
    let mut params: Vec<DbValue> = vec![
        DbValue::Text(id.to_string()),
        DbValue::Text(target.parent_id.to_string()),
        DbValue::Integer(order),
    ];

    if let Some(loc) = target.locale {
        cols.push("_locale".into());
        params.push(DbValue::Text(loc.to_string()));
    }

    for &sf in flat_subs {
        let (val, tz) = coerce_array_field(sf, row);
        cols.push(sf.name.clone());
        params.push(val);
        if let Some(tz_val) = tz {
            cols.push(tz_column(&sf.name));
            params.push(tz_val);
        }
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

/// UPDATE an existing array row, setting `_order` and ONLY the sub-field
/// columns present in the incoming row. A sub-field absent from the row —
/// removed by the write-access strip, or simply not sent — is left out of the
/// `SET`, so its stored value is preserved (the array analog of the scalar
/// update's set-only-present-columns rule).
fn update_array_row(
    conn: &dyn DbConnection,
    table_name: &str,
    id: &str,
    order: i64,
    row: &HashMap<String, Value>,
    flat_subs: &[&FieldDefinition],
) -> Result<()> {
    let mut set_parts: Vec<String> = vec![format!("_order = {}", conn.placeholder(1))];
    let mut params: Vec<DbValue> = vec![DbValue::Integer(order)];
    let mut idx = 2;

    for &sf in flat_subs {
        if !row.contains_key(&sf.name) {
            continue;
        }

        let (val, tz) = coerce_array_field(sf, row);
        set_parts.push(format!("{} = {}", sf.name, conn.placeholder(idx)));
        params.push(val);
        idx += 1;

        if let Some(tz_val) = tz {
            set_parts.push(format!(
                "{} = {}",
                tz_column(&sf.name),
                conn.placeholder(idx)
            ));
            params.push(tz_val);
            idx += 1;
        }
    }

    let where_ph = conn.placeholder(idx);
    params.push(DbValue::Text(id.to_string()));

    let sql = format!(
        "UPDATE \"{table_name}\" SET {} WHERE id = {where_ph}",
        set_parts.join(", ")
    );

    conn.execute(&sql, &params)?;
    Ok(())
}

/// Set array rows for an array field join table via a diff-based, per-row
/// column-preserving write.
///
/// An incoming row that carries an `id` matching an existing row of this
/// parent (and locale) UPDATEs that row, setting only the columns it supplies
/// — so a write-denied or absent sub-field keeps its stored value, exactly as
/// the scalar update path preserves an unlisted column. A row with no id, or an
/// id that is not an existing row, INSERTs a new row with a server-minted id
/// (a client can neither choose a primary key nor address another parent's
/// row). Rows the incoming set drops are deleted. `_order` follows the incoming
/// position. When `locale` is Some, the whole diff is scoped to that locale.
///
/// # Errors
///
/// Returns a backend error if any DELETE, UPDATE, or INSERT fails.
pub fn set_array_rows(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    rows: &[HashMap<String, Value>],
    sub_fields: &[FieldDefinition],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = join_table(collection, field_name);
    let flat_subs = flatten_array_sub_fields(sub_fields);

    // A degenerate array with no scalar columns has nothing to preserve or
    // diff — clear the parent's rows, matching the historical behavior.
    if flat_subs.is_empty() {
        return delete_junction_rows_except(conn, &table_name, parent_id, locale, &HashSet::new());
    }

    let existing_ids = existing_junction_ids(conn, &table_name, parent_id, locale)?;

    // Plan each row's identity: reuse the id when it names an existing row of
    // this parent[+locale] and hasn't already been claimed this write (→ UPDATE),
    // else mint a fresh id (→ INSERT). Row indices saturate at i64::MAX for the
    // unreachable case of >9.2e18 rows.
    let mut keep: HashSet<String> = HashSet::with_capacity(rows.len());
    let mut planned: Vec<(String, bool, i64)> = Vec::with_capacity(rows.len());
    for (order, row) in rows.iter().enumerate() {
        let order_i64 = i64::try_from(order).unwrap_or(i64::MAX);
        let (id, is_update) = match row.get("id").and_then(Value::as_str) {
            Some(cid) if existing_ids.contains(cid) && !keep.contains(cid) => {
                (cid.to_string(), true)
            }
            _ => (nanoid::nanoid!(), false),
        };
        keep.insert(id.clone());
        planned.push((id, is_update, order_i64));
    }

    delete_junction_rows_except(conn, &table_name, parent_id, locale, &keep)?;

    let target = JunctionTarget {
        table_name: &table_name,
        parent_id,
        locale,
    };

    for ((id, is_update, order), row) in planned.into_iter().zip(rows.iter()) {
        if is_update {
            update_array_row(conn, &table_name, &id, order, row, &flat_subs)?;
        } else {
            insert_array_row(conn, &target, &id, order, row, &flat_subs)?;
        }
    }

    Ok(())
}

/// Find array rows for an array field join table, ordered.
/// When `locale` is Some, filters by `_locale`.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn find_array_rows(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    sub_fields: &[FieldDefinition],
    locale: Option<&str>,
) -> Result<Vec<Value>> {
    let table_name = join_table(collection, field_name);
    let flat_subs = flatten_array_sub_fields(sub_fields);

    // Build SELECT column list including _tz companions
    let mut select_col_names: Vec<String> = Vec::new();
    for sf in &flat_subs {
        select_col_names.push(sf.name.clone());
        if sf.field_type == FieldType::Date && sf.timezone {
            select_col_names.push(tz_column(&sf.name));
        }
    }
    let select_cols = if select_col_names.is_empty() {
        "id".to_string()
    } else {
        format!("id, {}", select_col_names.join(", "))
    };
    let (sql, params) = select_junction_rows(conn, &table_name, &select_cols, parent_id, locale);

    let db_rows = conn.query_all(&sql, &params)?;
    let mut result = Vec::with_capacity(db_rows.len());

    for db_row in &db_rows {
        let mut map = reconstruct_array_row(db_row, &flat_subs, 1);

        if let Some(DbValue::Text(s)) = db_row.get_value(0) {
            map.insert("id".to_string(), Value::String(s.clone()));
        }

        result.push(Value::Object(map));
    }
    Ok(result)
}

/// Batched twin of [`find_array_rows`]: read the array rows of MANY parents
/// in one `IN (…)` query and bucket them per parent (ordered — the shared
/// batch SELECT orders by `parent_id, _order`). Parents with no rows are
/// absent from the map, which is what lets the caller's locale-fallback
/// pass target exactly the empty parents.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub fn find_array_rows_batch(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_ids: &[&str],
    sub_fields: &[FieldDefinition],
    locale: Option<&str>,
) -> Result<HashMap<String, Vec<Value>>> {
    if parent_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let table_name = join_table(collection, field_name);
    let flat_subs = flatten_array_sub_fields(sub_fields);

    // Same column list as the per-parent path, with `parent_id` spliced in
    // at index 1 for bucketing (sub-field decoding starts at index 2).
    let mut select_col_names: Vec<String> = Vec::new();
    for sf in &flat_subs {
        select_col_names.push(sf.name.clone());
        if sf.field_type == FieldType::Date && sf.timezone {
            select_col_names.push(tz_column(&sf.name));
        }
    }
    let select_cols = if select_col_names.is_empty() {
        "id, parent_id".to_string()
    } else {
        format!("id, parent_id, {}", select_col_names.join(", "))
    };

    let (sql, params) =
        select_junction_rows_batch(conn, &table_name, &select_cols, parent_ids, locale);

    let db_rows = conn.query_all(&sql, &params)?;
    let mut out: HashMap<String, Vec<Value>> = HashMap::new();

    for db_row in &db_rows {
        let Some(DbValue::Text(parent)) = db_row.get_value(1) else {
            continue;
        };

        let mut map = reconstruct_array_row(db_row, &flat_subs, 2);

        if let Some(DbValue::Text(s)) = db_row.get_value(0) {
            map.insert("id".to_string(), Value::String(s.clone()));
        }

        out.entry(parent.clone())
            .or_default()
            .push(Value::Object(map));
    }

    Ok(out)
}

/// Whether a sub-field column holds JSON that must be parsed on read: any
/// composite (Group/Array/Blocks/layout wrapper/Json) or a has-many
/// relationship/upload (stored as a JSON id array in the column).
fn sub_field_stores_json(sf: &FieldDefinition) -> bool {
    matches!(
        sf.field_type,
        FieldType::Array
            | FieldType::Blocks
            | FieldType::Group
            | FieldType::Row
            | FieldType::Collapsible
            | FieldType::Tabs
            | FieldType::Json
    ) || (sf.field_type.is_reference() && sf.relationship.as_ref().is_some_and(|rc| rc.has_many))
}

/// Reconstruct an array-row object from a DB row's sub-field columns, starting
/// at column index `start`. Composite sub-fields (Group/Array/Blocks/layout
/// wrappers/Json) are stored as JSON in TEXT columns and parsed back to
/// structured values, so nested composites at any depth come back ready for a
/// JSON walk. Date+timezone fields read their `_tz` companion column.
///
/// Shared by [`find_array_rows`] (per-parent read) and
/// [`find_all_array_rows_with_parent`] (back-reference scan) so the
/// column→JSON mapping lives in exactly one place.
pub(crate) fn reconstruct_array_row(
    db_row: &DbRow,
    flat_subs: &[&FieldDefinition],
    start: usize,
) -> Map<String, Value> {
    let mut map = Map::new();
    let mut col_idx = start;

    for sf in flat_subs {
        let val = db_row.get_value(col_idx).cloned().unwrap_or(DbValue::Null);
        col_idx += 1;

        let json_val = match val {
            DbValue::Integer(n) => json!(n),
            // A whole-valued Number must read back as an integer (`5`, not
            // `5.0`) — the same normalization every other read surface applies
            // via `real_to_json_number`. Number columns are floating-point, so
            // `5` round-trips through the DB as `5.0`.
            DbValue::Real(f) => real_to_json_number(f),
            DbValue::Text(s) if sub_field_stores_json(sf) => {
                // Composite sub-fields (and has-many relationship/upload, which
                // store a JSON id array) keep JSON in a TEXT column — parse it
                // so nested data comes back structured.
                serde_json::from_str(&s).unwrap_or(Value::String(s))
            }
            DbValue::Text(s) => Value::String(s),
            DbValue::Null | DbValue::Blob(_) => Value::Null,
        };
        map.insert(sf.name.clone(), json_val);

        if sf.field_type == FieldType::Date && sf.timezone {
            let tz_val = db_row.get_value(col_idx).cloned().unwrap_or(DbValue::Null);
            col_idx += 1;

            let tz_json = match tz_val {
                DbValue::Text(s) => Value::String(s),
                _ => Value::Null,
            };
            map.insert(tz_column(&sf.name), tz_json);
        }
    }

    map
}

/// Load every row of an array join table as `(parent_id, row_object)`, with
/// composite sub-fields JSON-parsed (see [`reconstruct_array_row`]). Used by
/// the back-reference scanner to walk array rows for nested relationships at
/// any depth (group-in-array, array-in-array, has-many in array) rather than
/// querying a single column. Not locale-scoped: a reference exists regardless
/// of which locale's row holds it.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails.
pub(crate) fn find_all_array_rows_with_parent(
    conn: &dyn DbConnection,
    array_table: &str,
    sub_fields: &[FieldDefinition],
) -> Result<Vec<(String, Map<String, Value>)>> {
    let flat_subs = flatten_array_sub_fields(sub_fields);

    let mut select_col_names: Vec<String> = Vec::new();
    for sf in &flat_subs {
        select_col_names.push(sf.name.clone());
        if sf.field_type == FieldType::Date && sf.timezone {
            select_col_names.push(tz_column(&sf.name));
        }
    }

    let select_cols = if select_col_names.is_empty() {
        "parent_id".to_string()
    } else {
        format!("parent_id, {}", select_col_names.join(", "))
    };
    let sql = format!("SELECT {select_cols} FROM \"{array_table}\"");

    let db_rows = conn.query_all(&sql, &[])?;
    let mut result = Vec::with_capacity(db_rows.len());

    for db_row in &db_rows {
        let Some(DbValue::Text(parent_id)) = db_row.get_value(0) else {
            continue;
        };
        let row = reconstruct_array_row(db_row, &flat_subs, 1);

        result.push((parent_id.clone(), row));
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::core::FieldTab;
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

    fn setup_array_db() -> (TempDir, BoxedConnection) {
        setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 label TEXT,
                 value TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        )
    }

    fn array_sub_fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("label", FieldType::Text).build(),
            FieldDefinition::builder("value", FieldType::Text).build(),
        ]
    }

    // ── set_array_rows + find_array_rows ─────────────────────────────────────

    #[test]
    fn set_and_find_array_rows() {
        let (_dir, conn) = setup_array_db();
        let sub = array_sub_fields();
        let rows = vec![
            HashMap::from([
                ("label".to_string(), json!("Label A")),
                ("value".to_string(), json!("Value A")),
            ]),
            HashMap::from([
                ("label".to_string(), json!("Label B")),
                ("value".to_string(), json!("Value B")),
            ]),
        ];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["label"], "Label A");
        assert_eq!(found[0]["value"], "Value A");
        assert_eq!(found[1]["label"], "Label B");
        assert_eq!(found[1]["value"], "Value B");
        assert!(found[0]["id"].as_str().is_some(), "Row should have an id");
        assert!(found[1]["id"].as_str().is_some(), "Row should have an id");
    }

    /// Seed two array rows and return their assigned ids in order.
    fn seed_two_rows(conn: &dyn DbConnection, sub: &[FieldDefinition]) -> (String, String) {
        let rows = vec![
            HashMap::from([
                ("label".to_string(), json!("A")),
                ("value".to_string(), json!("va")),
            ]),
            HashMap::from([
                ("label".to_string(), json!("B")),
                ("value".to_string(), json!("vb")),
            ]),
        ];
        set_array_rows(conn, "posts", "items", "p1", &rows, sub, None).unwrap();
        let found = find_array_rows(conn, "posts", "items", "p1", sub, None).unwrap();
        (
            found[0]["id"].as_str().unwrap().to_string(),
            found[1]["id"].as_str().unwrap().to_string(),
        )
    }

    /// The founding fix: an UPDATE that matches an existing row by `id` and
    /// OMITS a sub-field (as the write-access strip would) preserves that
    /// sub-field's stored value instead of clearing it. Rows absent from the
    /// incoming set are deleted.
    #[test]
    fn set_array_rows_diff_preserves_omitted_column_on_matched_id() {
        let (_dir, conn) = setup_array_db();
        let sub = array_sub_fields();
        let (id0, _id1) = seed_two_rows(&conn, &sub);

        // Update only row 0 by id, changing `label`, omitting `value`; drop row 1.
        let update = vec![HashMap::from([
            ("id".to_string(), json!(id0)),
            ("label".to_string(), json!("A2")),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &update, &sub, None).unwrap();

        let after = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(after.len(), 1, "row absent from the update is deleted");
        assert_eq!(
            after[0]["id"].as_str().unwrap(),
            id0,
            "matched row keeps its id"
        );
        assert_eq!(after[0]["label"], "A2", "supplied column is updated");
        assert_eq!(
            after[0]["value"], "va",
            "omitted column is PRESERVED, not cleared"
        );
    }

    /// A present-but-null sub-field on a matched row clears the column (parity
    /// with the scalar write rule: present writes, absent preserves).
    #[test]
    fn set_array_rows_diff_present_null_clears_column() {
        let (_dir, conn) = setup_array_db();
        let sub = array_sub_fields();
        let (id0, _id1) = seed_two_rows(&conn, &sub);

        let update = vec![HashMap::from([
            ("id".to_string(), json!(id0)),
            ("label".to_string(), json!("A2")),
            ("value".to_string(), Value::Null),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &update, &sub, None).unwrap();

        let after = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert!(
            after[0]["value"].is_null(),
            "an explicit null clears the column"
        );
    }

    /// Reordering by id preserves each row's identity and its omitted columns —
    /// no positional value bleed.
    #[test]
    fn set_array_rows_diff_reorder_preserves_values() {
        let (_dir, conn) = setup_array_db();
        let sub = array_sub_fields();
        let (id0, id1) = seed_two_rows(&conn, &sub);

        // Swap order, omit `value` on both.
        let update = vec![
            HashMap::from([
                ("id".to_string(), json!(id1)),
                ("label".to_string(), json!("B")),
            ]),
            HashMap::from([
                ("id".to_string(), json!(id0)),
                ("label".to_string(), json!("A")),
            ]),
        ];
        set_array_rows(&conn, "posts", "items", "p1", &update, &sub, None).unwrap();

        let after = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(after[0]["id"].as_str().unwrap(), id1);
        assert_eq!(
            after[0]["value"], "vb",
            "B's value follows its id, not its slot"
        );
        assert_eq!(after[1]["id"].as_str().unwrap(), id0);
        assert_eq!(after[1]["value"], "va");
    }

    /// An incoming id that is not an existing row of this parent is treated as a
    /// NEW row with a server-minted id — a client can neither choose a primary
    /// key nor address another row.
    #[test]
    fn set_array_rows_diff_unknown_id_becomes_new_row() {
        let (_dir, conn) = setup_array_db();
        let sub = array_sub_fields();
        let (id0, _id1) = seed_two_rows(&conn, &sub);

        let update = vec![HashMap::from([
            ("id".to_string(), json!("forged-id")),
            ("label".to_string(), json!("X")),
            ("value".to_string(), json!("vx")),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &update, &sub, None).unwrap();

        let after = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(after.len(), 1);
        assert_ne!(
            after[0]["id"].as_str().unwrap(),
            "forged-id",
            "a client-supplied unknown id never becomes the primary key"
        );
        assert_eq!(after[0]["label"], "X");
        assert!(
            after.iter().all(|r| r["id"].as_str() != Some(id0.as_str())),
            "the previous rows (absent from the update) are gone"
        );
    }

    /// Rows without an `id` (a surface that does not yet round-trip it, or a
    /// fresh create) behave exactly as before: full replace.
    #[test]
    fn set_array_rows_without_ids_replaces_all() {
        let (_dir, conn) = setup_array_db();
        let sub = array_sub_fields();
        let (id0, id1) = seed_two_rows(&conn, &sub);

        let update = vec![HashMap::from([
            ("label".to_string(), json!("C")),
            ("value".to_string(), json!("vc")),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &update, &sub, None).unwrap();

        let after = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0]["label"], "C");
        let new_id = after[0]["id"].as_str().unwrap();
        assert!(new_id != id0 && new_id != id1, "no-id rows get fresh ids");
    }

    /// Regression: a whole-valued Number sub-field in an array must read back
    /// as an integer (`5`), the same as a top-level Number field — not `5.0`.
    /// Number columns are floating-point, so the array read path must apply
    /// `real_to_json_number` like every other surface.
    #[test]
    fn whole_number_array_sub_field_reads_back_as_integer() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 qty REAL
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );
        let sub = vec![FieldDefinition::builder("qty", FieldType::Number).build()];

        let rows = vec![HashMap::from([("qty".to_string(), json!(5))])];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(found.len(), 1);
        // `json!(5)` (integer) and `json!(5.0)` (float) are distinct serde_json
        // Numbers, so this pins the integer shape.
        assert_eq!(found[0]["qty"], json!(5));
        assert_ne!(found[0]["qty"], json!(5.0));
    }

    #[test]
    fn replace_array_rows() {
        let (_dir, conn) = setup_array_db();
        let sub = array_sub_fields();
        let rows_old = vec![HashMap::from([
            ("label".to_string(), json!("Old")),
            ("value".to_string(), json!("Old Val")),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &rows_old, &sub, None).unwrap();

        let rows_new = vec![HashMap::from([
            ("label".to_string(), json!("New")),
            ("value".to_string(), json!("New Val")),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &rows_new, &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(found.len(), 1, "Old rows should be replaced");
        assert_eq!(found[0]["label"], "New");
        assert_eq!(found[0]["value"], "New Val");
    }

    #[test]
    fn empty_array_rows() {
        let (_dir, conn) = setup_array_db();
        let sub = array_sub_fields();
        let rows = vec![HashMap::from([
            ("label".to_string(), json!("X")),
            ("value".to_string(), json!("Y")),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();
        set_array_rows(&conn, "posts", "items", "p1", &[], &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert!(
            found.is_empty(),
            "Should return empty after setting empty rows"
        );
    }

    #[test]
    fn set_and_find_array_rows_with_tabs() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 title TEXT,
                 body TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        // Sub-fields wrapped in Tabs
        let sub_fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new(
                        "General",
                        vec![FieldDefinition::builder("title", FieldType::Text).build()],
                    ),
                    FieldTab::new(
                        "Content",
                        vec![FieldDefinition::builder("body", FieldType::Text).build()],
                    ),
                ])
                .build(),
        ];

        let mut row = HashMap::new();
        row.insert("title".to_string(), json!("Hello"));
        row.insert("body".to_string(), json!("World"));
        set_array_rows(&conn, "posts", "items", "p1", &[row], &sub_fields, None).unwrap();

        let result = find_array_rows(&conn, "posts", "items", "p1", &sub_fields, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["title"], "Hello");
        assert_eq!(result[0]["body"], "World");
    }

    #[test]
    fn set_and_find_array_rows_with_row_wrapper() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 x TEXT,
                 y TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let sub_fields = vec![
            FieldDefinition::builder("row_wrap", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("x", FieldType::Text).build(),
                    FieldDefinition::builder("y", FieldType::Text).build(),
                ])
                .build(),
        ];

        let mut row = HashMap::new();
        row.insert("x".to_string(), json!("10"));
        row.insert("y".to_string(), json!("20"));
        set_array_rows(&conn, "posts", "items", "p1", &[row], &sub_fields, None).unwrap();

        let result = find_array_rows(&conn, "posts", "items", "p1", &sub_fields, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["x"], "10");
        assert_eq!(result[0]["y"], "20");
    }

    #[test]
    fn find_array_rows_empty_sub_fields_returns_only_id() {
        // When there are no sub-fields, set_array_rows returns early (no rows inserted).
        // find_array_rows with empty sub_fields selects only "id" column.
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_items (id, parent_id, _order) VALUES ('item1', 'p1', 0);",
        );

        let result = find_array_rows(&conn, "posts", "items", "p1", &[], None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["id"], "item1");
    }

    // ── Timezone companion tests ─────────────────────────────────────

    #[test]
    fn set_and_find_array_rows_with_date_timezone() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_schedule (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 event_date TEXT,
                 event_date_tz TEXT,
                 label TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let sub_fields = vec![
            FieldDefinition::builder("event_date", FieldType::Date)
                .timezone(true)
                .build(),
            FieldDefinition::builder("label", FieldType::Text).build(),
        ];

        let rows = vec![HashMap::from([
            ("event_date".to_string(), json!("2024-01-15T09:00")),
            ("event_date_tz".to_string(), json!("America/New_York")),
            ("label".to_string(), json!("Meeting")),
        ])];

        set_array_rows(&conn, "posts", "schedule", "p1", &rows, &sub_fields, None).unwrap();

        let found = find_array_rows(&conn, "posts", "schedule", "p1", &sub_fields, None).unwrap();
        assert_eq!(found.len(), 1);

        // 9am EST = 2pm UTC
        assert_eq!(found[0]["event_date"], "2024-01-15T14:00:00.000Z");
        assert_eq!(found[0]["event_date_tz"], "America/New_York");
        assert_eq!(found[0]["label"], "Meeting");
    }

    #[test]
    fn set_array_rows_date_tz_without_tz_value() {
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_schedule (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 event_date TEXT,
                 event_date_tz TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let sub_fields = vec![
            FieldDefinition::builder("event_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        let rows = vec![HashMap::from([(
            "event_date".to_string(),
            json!("2024-01-15T09:00"),
        )])];

        set_array_rows(&conn, "posts", "schedule", "p1", &rows, &sub_fields, None).unwrap();

        let found = find_array_rows(&conn, "posts", "schedule", "p1", &sub_fields, None).unwrap();
        assert_eq!(found.len(), 1);

        // No timezone provided — falls back to treat as UTC
        assert_eq!(found[0]["event_date"], "2024-01-15T09:00:00.000Z");
        assert!(
            found[0]["event_date_tz"].is_null(),
            "tz should be null when not provided"
        );
    }

    // ── Deep nesting round-trips (write → DB → read) ─────────────────

    #[test]
    fn array_in_array_round_trips_as_structured_json() {
        // An array nested inside an array row is stored as JSON in the outer
        // row's column and must come back as a structured array, not a string.
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_outer (
                 id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, inner TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let sub_fields = vec![
            FieldDefinition::builder("inner", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                ])
                .build(),
        ];
        let rows = vec![HashMap::from([(
            "inner".to_string(),
            json!([{ "label": "a" }, { "label": "b" }]),
        )])];

        set_array_rows(&conn, "posts", "outer", "p1", &rows, &sub_fields, None).unwrap();
        let found = find_array_rows(&conn, "posts", "outer", "p1", &sub_fields, None).unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0]["inner"],
            json!([{ "label": "a" }, { "label": "b" }])
        );
    }

    #[test]
    fn has_many_relationship_in_array_round_trips_as_array() {
        // A has-many relationship inside an array row stores its id list as JSON
        // in the column and must read back as an array (not a raw string).
        let (_dir, conn) = setup_conn(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_rows (
                 id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, tags TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');",
        );

        let sub_fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(crate::core::RelationshipConfig::new("tags", true))
                .has_many(true)
                .build(),
        ];
        let rows = vec![HashMap::from([("tags".to_string(), json!(["t1", "t2"]))])];

        set_array_rows(&conn, "posts", "rows", "p1", &rows, &sub_fields, None).unwrap();
        let found = find_array_rows(&conn, "posts", "rows", "p1", &sub_fields, None).unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["tags"], json!(["t1", "t2"]));
    }
}
