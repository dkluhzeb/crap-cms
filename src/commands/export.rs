//! `export` and `import` commands — collection data import/export as JSON.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Export collection data to JSON.
// Excluded from coverage: requires full Lua + DB setup via load_config_and_sync.
// Tested via CLI integration tests in tests/cli_integration.rs.
#[cfg(not(tarpaulin_include))]
pub fn export(
    config_dir: &Path,
    collection_filter: Option<String>,
    output: Option<PathBuf>,
) -> Result<()> {
    let (pool, registry) = super::load_config_and_sync(config_dir)?;

    let reg = registry.read()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

    let conn = pool.get().context("Failed to get database connection")?;

    let mut collections_data = serde_json::Map::new();

    let slugs: Vec<String> = if let Some(ref slug) = collection_filter {
        if reg.get_collection(slug).is_none() {
            anyhow::bail!("Collection '{}' not found", slug);
        }
        vec![slug.clone()]
    } else {
        let mut s: Vec<_> = reg.collections.keys().cloned().collect();
        s.sort();
        s
    };

    for slug in &slugs {
        let def = &reg.collections[slug];

        let query = crate::db::query::FindQuery {
            filters: vec![],
            order_by: None,
            limit: None,
            offset: None,
            select: None,
        };

        let mut docs = crate::db::query::find(&conn, slug, def, &query, None)?;

        for doc in &mut docs {
            crate::db::query::hydrate_document(&conn, slug, &def.fields, doc, None, None)?;
        }

        let docs_json: Vec<serde_json::Value> = docs.into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;

        collections_data.insert(slug.clone(), serde_json::Value::Array(docs_json));
    }

    let output_json = serde_json::json!({ "collections": collections_data });

    match output {
        Some(path) => {
            let content = serde_json::to_string_pretty(&output_json)?;
            std::fs::write(&path, content)
                .with_context(|| format!("Failed to write {}", path.display()))?;
            eprintln!("Exported {} collection(s) to {}", slugs.len(), path.display());
        }
        None => {
            println!("{}", serde_json::to_string_pretty(&output_json)?);
        }
    }

    Ok(())
}

/// Import collection data from JSON.
// Excluded from coverage: requires full Lua + DB setup via load_config_and_sync.
// Tested via CLI integration tests in tests/cli_integration.rs.
#[cfg(not(tarpaulin_include))]
pub fn import(
    config_dir: &Path,
    file: &Path,
    collection_filter: Option<String>,
) -> Result<()> {
    let (pool, registry) = super::load_config_and_sync(config_dir)?;

    let content = std::fs::read_to_string(file)
        .with_context(|| format!("Failed to read {}", file.display()))?;
    let data: serde_json::Value = serde_json::from_str(&content)
        .context("Failed to parse JSON")?;

    let collections_obj = data.get("collections")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow::anyhow!("Expected top-level \"collections\" object in JSON"))?;

    let reg = registry.read()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

    let slugs: Vec<String> = if let Some(ref slug) = collection_filter {
        if !collections_obj.contains_key(slug) {
            anyhow::bail!("Collection '{}' not found in import file", slug);
        }
        vec![slug.clone()]
    } else {
        collections_obj.keys().cloned().collect()
    };

    let mut total_imported = 0usize;

    for slug in &slugs {
        let def = reg.get_collection(slug)
            .ok_or_else(|| anyhow::anyhow!("Collection '{}' exists in import file but not in schema", slug))?;

        let docs_array = collections_obj.get(slug)
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Expected array for collection '{}'", slug))?;

        let mut conn = pool.get().context("Failed to get database connection")?;
        let tx = conn.transaction().context("Failed to begin transaction")?;

        for doc_val in docs_array {
            let doc_obj = doc_val.as_object()
                .ok_or_else(|| anyhow::anyhow!("Expected document object in '{}'", slug))?;

            let id = doc_obj.get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Document missing 'id' in '{}'", slug))?;

            // Separate parent-column fields from join-table fields
            let mut parent_cols: Vec<String> = vec!["id".to_string()];
            let mut parent_vals: Vec<String> = vec![id.to_string()];
            let mut join_data: HashMap<String, serde_json::Value> = HashMap::new();

            // Handle timestamps
            if def.timestamps {
                if let Some(v) = doc_obj.get("created_at").and_then(|v| v.as_str()) {
                    parent_cols.push("created_at".to_string());
                    parent_vals.push(v.to_string());
                }
                if let Some(v) = doc_obj.get("updated_at").and_then(|v| v.as_str()) {
                    parent_cols.push("updated_at".to_string());
                    parent_vals.push(v.to_string());
                }
            }

            for field in &def.fields {
                if field.has_parent_column() {
                    if field.field_type == crate::core::field::FieldType::Group {
                        // Group fields have prefixed columns: group__sub
                        continue; // handled below
                    }
                    // Try direct key first, then flattened
                    if let Some(val) = doc_obj.get(&field.name) {
                        let str_val = match val {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Null => continue,
                            other => other.to_string(),
                        };
                        parent_cols.push(field.name.clone());
                        parent_vals.push(str_val);
                    }
                } else {
                    // Join table fields (array, blocks, has-many relationship)
                    if let Some(val) = doc_obj.get(&field.name) {
                        if !val.is_null() {
                            join_data.insert(field.name.clone(), val.clone());
                        }
                    }
                }

                // Handle group sub-fields (they use parent columns with prefix)
                if field.field_type == crate::core::field::FieldType::Group {
                    for sub in &field.fields {
                        let col_name = format!("{}__{}", field.name, sub.name);
                        // Try nested object first (hydrated export format)
                        let val = doc_obj.get(&field.name)
                            .and_then(|g| g.get(&sub.name))
                            // Then try flattened format
                            .or_else(|| doc_obj.get(&col_name));

                        if let Some(val) = val {
                            let str_val = match val {
                                serde_json::Value::String(s) => s.clone(),
                                serde_json::Value::Null => continue,
                                other => other.to_string(),
                            };
                            parent_cols.push(col_name);
                            parent_vals.push(str_val);
                        }
                    }
                }
            }

            // INSERT OR REPLACE
            let placeholders: Vec<String> = (0..parent_cols.len()).map(|i| format!("?{}", i + 1)).collect();
            let sql = format!(
                "INSERT OR REPLACE INTO \"{}\" ({}) VALUES ({})",
                slug,
                parent_cols.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(", "),
                placeholders.join(", ")
            );

            let params: Vec<Box<dyn rusqlite::types::ToSql>> = parent_vals.iter()
                .map(|v| Box::new(v.clone()) as Box<dyn rusqlite::types::ToSql>)
                .collect();
            let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter()
                .map(|p| p.as_ref())
                .collect();

            tx.execute(&sql, param_refs.as_slice())
                .with_context(|| format!("Failed to insert document {} into '{}'", id, slug))?;

            // Save join table data
            if !join_data.is_empty() {
                crate::db::query::save_join_table_data(&tx, slug, &def.fields, id, &join_data, None)?;
            }

            total_imported += 1;
        }

        tx.commit()
            .with_context(|| format!("Failed to commit import for '{}'", slug))?;

        println!("Imported {} document(s) into '{}'", docs_array.len(), slug);
    }

    println!("\nTotal: {} document(s) imported", total_imported);

    Ok(())
}
