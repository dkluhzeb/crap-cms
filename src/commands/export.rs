//! `export` and `import` commands — collection data import/export as JSON.

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use super::load_config_and_sync;
use crate::{
    cli,
    config::CrapConfig,
    core::{CollectionDefinition, FieldType},
    db::{DbConnection, DbValue, query},
};

/// Export collection data to JSON.
// Excluded from coverage: requires full Lua + DB setup via load_config_and_sync.
// Tested via CLI integration tests in tests/cli_integration.rs.
#[cfg(not(tarpaulin_include))]
pub fn export(
    config_dir: &Path,
    collection_filter: Option<String>,
    output: Option<PathBuf>,
) -> Result<()> {
    let (pool, registry) = load_config_and_sync(config_dir)?;

    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let conn = pool.get().context("Failed to get database connection")?;

    let mut collections_data = Map::new();

    let slugs: Vec<String> = if let Some(ref slug) = collection_filter {
        if reg.get_collection(slug).is_none() {
            bail!("Collection '{}' not found", slug);
        }

        vec![slug.clone()]
    } else {
        let mut s: Vec<String> = reg.collections.keys().map(|s| s.to_string()).collect();
        s.sort();

        s
    };

    for slug in &slugs {
        let def = &reg.collections[slug.as_str()];

        let find_query = query::FindQuery::default();

        let mut docs = query::find(&conn, slug, def, &find_query, None)?;

        for doc in &mut docs {
            query::hydrate_document(&conn, slug, &def.fields, doc, None, None)?;
        }

        let docs_json: Vec<Value> = docs
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;

        collections_data.insert(slug.clone(), Value::Array(docs_json));
    }

    let output_json = json!({
        "crap_version": env!("CARGO_PKG_VERSION"),
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "collections": collections_data,
    });

    match output {
        Some(path) => {
            let content = serde_json::to_string_pretty(&output_json)?;

            fs::write(&path, content)
                .with_context(|| format!("Failed to write {}", path.display()))?;

            cli::success(&format!(
                "Exported {} collection(s) to {}",
                slugs.len(),
                path.display()
            ));
        }
        None => {
            println!("{}", serde_json::to_string_pretty(&output_json)?);
        }
    }

    Ok(())
}

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

/// Collect parent columns and join data for a single document from its JSON representation.
fn collect_import_columns(
    doc_obj: &Map<String, Value>,
    def: &CollectionDefinition,
    id: &str,
) -> ImportRow {
    let mut parent_cols: Vec<String> = vec!["id".to_string()];
    let mut parent_vals: Vec<DbValue> = vec![DbValue::Text(id.to_string())];
    let mut join_data: HashMap<String, Value> = HashMap::new();

    // Handle timestamps (always text)
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
        if field.has_parent_column() && field.field_type != FieldType::Group {
            if let Some(val) = doc_obj.get(&field.name) {
                push_field_value(
                    &mut parent_cols,
                    &mut parent_vals,
                    field.name.clone(),
                    val,
                    &field.field_type,
                );
            }
        } else if !field.has_parent_column() {
            // Join table fields (array, blocks, has-many relationship)
            if let Some(val) = doc_obj.get(&field.name)
                && !val.is_null()
            {
                join_data.insert(field.name.clone(), val.clone());
            }
        }

        // Handle group sub-fields (they use parent columns with prefix)
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                let col_name = format!("{}__{}", field.name, sub.name);

                // Try nested object first (hydrated export format)
                let val = doc_obj
                    .get(&field.name)
                    .and_then(|g| g.get(&sub.name))
                    // Then try flattened format
                    .or_else(|| doc_obj.get(&col_name));

                if let Some(val) = val {
                    push_field_value(
                        &mut parent_cols,
                        &mut parent_vals,
                        col_name,
                        val,
                        &sub.field_type,
                    );
                }
            }
        }

        // Handle row/collapsible sub-fields (parent columns, no prefix)
        if field.field_type == FieldType::Row || field.field_type == FieldType::Collapsible {
            for sub in &field.fields {
                if let Some(val) = doc_obj.get(&sub.name) {
                    push_field_value(
                        &mut parent_cols,
                        &mut parent_vals,
                        sub.name.clone(),
                        val,
                        &sub.field_type,
                    );
                }
            }
        }

        // Handle tabs sub-fields (parent columns, no prefix, across all tabs)
        if field.field_type == FieldType::Tabs {
            for tab in &field.tabs {
                for sub in &tab.fields {
                    if let Some(val) = doc_obj.get(&sub.name) {
                        push_field_value(
                            &mut parent_cols,
                            &mut parent_vals,
                            sub.name.clone(),
                            val,
                            &sub.field_type,
                        );
                    }
                }
            }
        }
    }

    ImportRow {
        parent_cols,
        parent_vals,
        join_data,
    }
}

/// Import collection data from JSON.
// Excluded from coverage: requires full Lua + DB setup via load_config_and_sync.
// Tested via CLI integration tests in tests/cli_integration.rs.
#[cfg(not(tarpaulin_include))]
pub fn import(config_dir: &Path, file: &Path, collection_filter: Option<String>) -> Result<()> {
    let (pool, registry) = load_config_and_sync(config_dir)?;

    let content =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;

    let data: Value = serde_json::from_str(&content).context("Failed to parse JSON")?;

    // Check version compatibility
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
            let doc_obj = doc_val
                .as_object()
                .ok_or_else(|| anyhow!("Expected document object in '{}'", slug))?;

            let id = doc_obj
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Document missing 'id' in '{}'", slug))?;

            let row = collect_import_columns(doc_obj, def, id);
            let parent_cols = row.parent_cols;
            let parent_vals = row.parent_vals;
            let join_data = row.join_data;

            // Upsert (INSERT OR REPLACE for SQLite, ON CONFLICT for Postgres)
            let placeholders: Vec<String> = (0..parent_cols.len())
                .map(|i| tx.placeholder(i + 1))
                .collect();
            let col_refs: Vec<&str> = parent_cols.iter().map(String::as_str).collect();
            let sql = tx.build_upsert(
                &format!("\"{}\"", slug),
                &col_refs,
                &placeholders.join(", "),
                "id",
            );

            tx.execute(&sql, &parent_vals)
                .with_context(|| format!("Failed to insert document {} into '{}'", id, slug))?;

            // Save join table data
            if !join_data.is_empty() {
                query::save_join_table_data(&tx, slug, &def.fields, id, &join_data, None)?;
            }

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
    use super::*;
    use serde_json::json;

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
