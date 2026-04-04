//! `import` command — load collection data from JSON.

use std::{collections::HashMap, fs, path::Path};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value};

use crate::{
    cli,
    commands::load_config_and_sync,
    config::CrapConfig,
    core::{CollectionDefinition, FieldDefinition, FieldType},
    db::{DbConnection, DbValue, query},
};

/// Collected columns for a single document import row.
struct ImportRow {
    parent_cols: Vec<String>,
    parent_vals: Vec<DbValue>,
    join_data: HashMap<String, Value>,
}

/// Convert a JSON value to a typed DbValue based on the field type.
fn json_to_db_value(val: &Value, field_type: &FieldType) -> Option<DbValue> {
    match val {
        Value::Null => None,
        Value::String(s) => Some(DbValue::Text(s.clone())),
        Value::Number(n) => match field_type {
            FieldType::Number => n.as_f64().map(DbValue::Real),
            _ => n
                .as_i64()
                .map(DbValue::Integer)
                .or_else(|| n.as_f64().map(DbValue::Real)),
        },
        Value::Bool(b) => Some(DbValue::Integer(if *b { 1 } else { 0 })),
        other => Some(DbValue::Text(other.to_string())),
    }
}

/// Push a column/value pair into the import row if the JSON value is non-null.
fn push_field_value(
    cols: &mut Vec<String>,
    vals: &mut Vec<DbValue>,
    col_name: String,
    val: &Value,
    field_type: &FieldType,
) {
    if let Some(db_val) = json_to_db_value(val, field_type) {
        cols.push(col_name);
        vals.push(db_val);
    }
}

/// Collect columns and join data for a single field, handling different field types.
fn collect_field_columns(
    field: &FieldDefinition,
    doc_obj: &Map<String, Value>,
    parent_cols: &mut Vec<String>,
    parent_vals: &mut Vec<DbValue>,
    join_data: &mut HashMap<String, Value>,
) {
    match field.field_type {
        FieldType::Group => {
            collect_group_columns(field, doc_obj, parent_cols, parent_vals);
        }
        FieldType::Row | FieldType::Collapsible => {
            collect_wrapper_columns(&field.fields, doc_obj, parent_cols, parent_vals);
        }
        FieldType::Tabs => {
            for tab in &field.tabs {
                collect_wrapper_columns(&tab.fields, doc_obj, parent_cols, parent_vals);
            }
        }
        _ if field.has_parent_column() => {
            if let Some(val) = doc_obj.get(&field.name) {
                push_field_value(
                    parent_cols,
                    parent_vals,
                    field.name.clone(),
                    val,
                    &field.field_type,
                );
            }
        }
        _ => {
            if let Some(val) = doc_obj.get(&field.name)
                && !val.is_null()
            {
                join_data.insert(field.name.clone(), val.clone());
            }
        }
    }
}

/// Collect group sub-fields as `group__subfield` parent columns.
fn collect_group_columns(
    field: &FieldDefinition,
    doc_obj: &Map<String, Value>,
    parent_cols: &mut Vec<String>,
    parent_vals: &mut Vec<DbValue>,
) {
    for sub in &field.fields {
        let col_name = format!("{}__{}", field.name, sub.name);

        let val = doc_obj
            .get(&field.name)
            .and_then(|g| g.get(&sub.name))
            .or_else(|| doc_obj.get(&col_name));

        if let Some(val) = val {
            push_field_value(parent_cols, parent_vals, col_name, val, &sub.field_type);
        }
    }
}

/// Collect sub-fields from layout wrappers (Row, Collapsible, Tabs) as parent columns.
fn collect_wrapper_columns(
    fields: &[FieldDefinition],
    doc_obj: &Map<String, Value>,
    parent_cols: &mut Vec<String>,
    parent_vals: &mut Vec<DbValue>,
) {
    for sub in fields {
        if let Some(val) = doc_obj.get(&sub.name) {
            push_field_value(
                parent_cols,
                parent_vals,
                sub.name.clone(),
                val,
                &sub.field_type,
            );
        }
    }
}

/// Collect parent columns and join data for a single document from its JSON representation.
fn collect_import_columns(
    doc_obj: &Map<String, Value>,
    def: &CollectionDefinition,
    id: &str,
) -> ImportRow {
    let mut parent_cols: Vec<String> = vec!["id".to_string()];
    let mut parent_vals: Vec<DbValue> = vec![DbValue::Text(id.to_string())];
    let mut join_data: HashMap<String, Value> = HashMap::new();

    if def.timestamps {
        if let Some(v) = doc_obj.get("created_at").and_then(|v| v.as_str()) {
            parent_cols.push("created_at".to_string());
            parent_vals.push(DbValue::Text(v.to_string()));
        }

        if let Some(v) = doc_obj.get("updated_at").and_then(|v| v.as_str()) {
            parent_cols.push("updated_at".to_string());
            parent_vals.push(DbValue::Text(v.to_string()));
        }
    }

    for field in &def.fields {
        collect_field_columns(
            field,
            doc_obj,
            &mut parent_cols,
            &mut parent_vals,
            &mut join_data,
        );
    }

    ImportRow {
        parent_cols,
        parent_vals,
        join_data,
    }
}

/// Import a single document into a collection via upsert + join table data.
fn import_single_document(
    doc_val: &Value,
    slug: &str,
    def: &CollectionDefinition,
    tx: &dyn DbConnection,
) -> Result<()> {
    let doc_obj = doc_val
        .as_object()
        .ok_or_else(|| anyhow!("Expected document object in '{}'", slug))?;

    let id = doc_obj
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Document missing 'id' in '{}'", slug))?;

    let row = collect_import_columns(doc_obj, def, id);

    let placeholders: Vec<String> = (0..row.parent_cols.len())
        .map(|i| tx.placeholder(i + 1))
        .collect();

    let col_refs: Vec<&str> = row.parent_cols.iter().map(String::as_str).collect();

    let sql = tx.build_upsert(
        &format!("\"{}\"", slug),
        &col_refs,
        &placeholders.join(", "),
        "id",
    );

    tx.execute(&sql, &row.parent_vals)
        .with_context(|| format!("Failed to insert document {} into '{}'", id, slug))?;

    if !row.join_data.is_empty() {
        query::save_join_table_data(tx, slug, &def.fields, id, &row.join_data, None)?;
    }

    Ok(())
}

/// Import collection data from JSON.
#[cfg(not(tarpaulin_include))]
pub fn import(config_dir: &Path, file: &Path, collection_filter: Option<String>) -> Result<()> {
    let (pool, registry) = load_config_and_sync(config_dir)?;

    let content =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;

    let data: Value = serde_json::from_str(&content).context("Failed to parse JSON")?;

    if let Some(export_version) = data.get("crap_version").and_then(|v| v.as_str()) {
        let current = env!("CARGO_PKG_VERSION");

        if let Some(warning) = CrapConfig::check_version_against(Some(export_version), current) {
            cli::warning(&warning.replace("config requires", "export file was created with"));
        }
    }

    let collections_obj = data
        .get("collections")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow!("Expected top-level \"collections\" object in JSON"))?;

    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let slugs: Vec<String> = if let Some(ref slug) = collection_filter {
        if !collections_obj.contains_key(slug) {
            bail!("Collection '{}' not found in import file", slug);
        }
        vec![slug.clone()]
    } else {
        collections_obj.keys().cloned().collect()
    };

    let mut total_imported = 0usize;

    for slug in &slugs {
        let def = reg.get_collection(slug).ok_or_else(|| {
            anyhow!(
                "Collection '{}' exists in import file but not in schema",
                slug
            )
        })?;

        let docs_array = collections_obj
            .get(slug)
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("Expected array for collection '{}'", slug))?;

        let mut conn = pool.get().context("Failed to get database connection")?;
        let tx = conn.transaction().context("Failed to begin transaction")?;

        for doc_val in docs_array {
            import_single_document(doc_val, slug, def, &tx)?;
            total_imported += 1;
        }

        tx.commit()
            .with_context(|| format!("Failed to commit import for '{}'", slug))?;

        cli::success(&format!(
            "Imported {} document(s) into '{}'",
            docs_array.len(),
            slug
        ));
    }

    cli::success(&format!("Total: {} document(s) imported", total_imported));

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn json_to_db_value_null() {
        assert!(json_to_db_value(&Value::Null, &FieldType::Text).is_none());
    }

    #[test]
    fn json_to_db_value_string() {
        let val = json_to_db_value(&json!("hello"), &FieldType::Text);
        assert!(matches!(val, Some(DbValue::Text(s)) if s == "hello"));
    }

    #[test]
    fn json_to_db_value_integer() {
        let val = json_to_db_value(&json!(42), &FieldType::Text);
        assert!(matches!(val, Some(DbValue::Integer(42))));
    }

    #[test]
    fn json_to_db_value_number_field_gives_real() {
        let val = json_to_db_value(&json!(42), &FieldType::Number);
        assert!(matches!(val, Some(DbValue::Real(v)) if (v - 42.0).abs() < f64::EPSILON));
    }

    #[test]
    fn json_to_db_value_float() {
        let val = json_to_db_value(&json!(2.5), &FieldType::Text);
        assert!(matches!(val, Some(DbValue::Real(v)) if (v - 2.5).abs() < f64::EPSILON));
    }

    #[test]
    fn json_to_db_value_bool_true() {
        let val = json_to_db_value(&json!(true), &FieldType::Checkbox);
        assert!(matches!(val, Some(DbValue::Integer(1))));
    }

    #[test]
    fn json_to_db_value_bool_false() {
        let val = json_to_db_value(&json!(false), &FieldType::Checkbox);
        assert!(matches!(val, Some(DbValue::Integer(0))));
    }

    #[test]
    fn json_to_db_value_object_becomes_text() {
        let val = json_to_db_value(&json!({"key": "val"}), &FieldType::Json);
        assert!(matches!(val, Some(DbValue::Text(_))));
    }
}
