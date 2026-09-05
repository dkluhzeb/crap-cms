//! `import` command — load collection data from JSON.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value};

use crate::{
    cli,
    commands::{export::file::ExportFile, load_config_and_sync},
    config::{CrapConfig, LocaleConfig},
    core::{CollectionDefinition, DocumentFields, FieldDefinition, FieldType, Registry},
    db::{
        DbConnection, DbValue,
        query::{self, helpers::prefixed_name},
    },
};

/// Collected columns for a single document import row.
struct ImportRow {
    parent_cols: Vec<String>,
    parent_vals: Vec<DbValue>,
    join_data: DocumentFields,
}

/// Convert a JSON value to a typed `DbValue` based on the field type.
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
        Value::Bool(b) => Some(DbValue::Integer(i64::from(*b))),
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
    join_data: &mut DocumentFields,
    locale: &LocaleConfig,
) -> Result<()> {
    match field.field_type {
        FieldType::Group => {
            collect_group_columns(field, doc_obj, parent_cols, parent_vals, locale)?;
        }
        FieldType::Row | FieldType::Collapsible => {
            collect_wrapper_columns(&field.fields, doc_obj, parent_cols, parent_vals, locale)?;
        }
        FieldType::Tabs => {
            for tab in &field.tabs {
                collect_wrapper_columns(&tab.fields, doc_obj, parent_cols, parent_vals, locale)?;
            }
        }
        _ if field.has_parent_column() => {
            if let Some(val) = doc_obj.get(&field.name) {
                if field.is_locale_scoped(false) {
                    push_localized_columns(
                        parent_cols,
                        parent_vals,
                        &field.name,
                        &field.name,
                        val,
                        &field.field_type,
                        locale,
                    )?;
                } else {
                    push_field_value(
                        parent_cols,
                        parent_vals,
                        field.name.clone(),
                        val,
                        &field.field_type,
                    );
                }
            }
        }
        _ => {
            if let Some(val) = doc_obj.get(&field.name)
                && !val.is_null()
            {
                // Join-backed fields (arrays/blocks/has-many) that are
                // themselves localized store per-locale rows; this raw
                // upsert has no per-locale join writer, and silently
                // importing under one locale would drop the others.
                if field.localized {
                    bail!(
                        "field '{}' is a localized {} field — `crap-cms import` does not \
                         support localized join-backed fields; import this collection \
                         through the API instead",
                        field.name,
                        field.field_type.as_str(),
                    );
                }

                join_data.insert(field.name.clone(), val.clone());
            }
        }
    }

    Ok(())
}

/// Write one localized parent-column field: the JSON value must be an object
/// of `locale → value`, each landing in its `{column}__{locale}` column.
/// Fail-fast on anything else — importing a bare scalar into a localized
/// field is ambiguous, and an unknown locale key would write a column that
/// does not exist.
#[allow(clippy::too_many_arguments)]
fn push_localized_columns(
    parent_cols: &mut Vec<String>,
    parent_vals: &mut Vec<DbValue>,
    field_name: &str,
    base_col: &str,
    val: &Value,
    field_type: &FieldType,
    locale: &LocaleConfig,
) -> Result<()> {
    let Value::Object(by_locale) = val else {
        bail!(
            "field '{field_name}' is localized — expected an object of locale → value \
             (e.g. {{\"{}\": ...}}), got {val}",
            locale.default_locale
        );
    };

    for (loc, v) in by_locale {
        if loc != &locale.default_locale && !locale.locales.contains(loc) {
            bail!(
                "field '{field_name}': unknown locale '{loc}' (configured: {:?})",
                locale.locales
            );
        }

        push_field_value(
            parent_cols,
            parent_vals,
            format!("{base_col}__{loc}"),
            v,
            field_type,
        );
    }

    Ok(())
}

/// Collect group sub-fields as `group__subfield` parent columns.
fn collect_group_columns(
    field: &FieldDefinition,
    doc_obj: &Map<String, Value>,
    parent_cols: &mut Vec<String>,
    parent_vals: &mut Vec<DbValue>,
    locale: &LocaleConfig,
) -> Result<()> {
    for sub in &field.fields {
        let col_name = prefixed_name(&field.name, &sub.name);

        let val = doc_obj
            .get(&field.name)
            .and_then(|g| g.get(&sub.name))
            .or_else(|| doc_obj.get(&col_name));

        if let Some(val) = val {
            if sub.is_locale_scoped(field.localized) {
                push_localized_columns(
                    parent_cols,
                    parent_vals,
                    &format!("{}.{}", field.name, sub.name),
                    &col_name,
                    val,
                    &sub.field_type,
                    locale,
                )?;
            } else {
                push_field_value(parent_cols, parent_vals, col_name, val, &sub.field_type);
            }
        }
    }

    Ok(())
}

/// Collect sub-fields from layout wrappers (Row, Collapsible, Tabs) as parent columns.
fn collect_wrapper_columns(
    fields: &[FieldDefinition],
    doc_obj: &Map<String, Value>,
    parent_cols: &mut Vec<String>,
    parent_vals: &mut Vec<DbValue>,
    locale: &LocaleConfig,
) -> Result<()> {
    for sub in fields {
        if let Some(val) = doc_obj.get(&sub.name) {
            if sub.is_locale_scoped(false) {
                push_localized_columns(
                    parent_cols,
                    parent_vals,
                    &sub.name,
                    &sub.name,
                    val,
                    &sub.field_type,
                    locale,
                )?;
            } else {
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

    Ok(())
}

/// Collect parent columns and join data for a single document from its JSON representation.
fn collect_import_columns(
    doc_obj: &Map<String, Value>,
    def: &CollectionDefinition,
    id: &str,
    locale: &LocaleConfig,
) -> Result<ImportRow> {
    let mut parent_cols: Vec<String> = vec!["id".to_string()];
    let mut parent_vals: Vec<DbValue> = vec![DbValue::Text(id.to_string())];
    let mut join_data = DocumentFields::new();

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
            locale,
        )?;
    }

    Ok(ImportRow {
        parent_cols,
        parent_vals,
        join_data,
    })
}

/// Import a single document into a collection via upsert + join table data.
///
/// Reference counts are kept consistent: outgoing refs are snapshotted
/// before the write (empty for a new document) and diffed afterwards, so
/// imported relationships participate in delete protection exactly like
/// documents written through the service layer.
fn import_single_document(
    doc_val: &Value,
    slug: &str,
    def: &CollectionDefinition,
    tx: &dyn DbConnection,
    locale: &LocaleConfig,
) -> Result<()> {
    let doc_obj = doc_val
        .as_object()
        .ok_or_else(|| anyhow!("Expected document object in '{slug}'"))?;

    let id = doc_obj
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Document missing 'id' in '{slug}'"))?;

    let old_refs = query::ref_count::snapshot_outgoing_refs(tx, slug, id, &def.fields, locale)
        .with_context(|| format!("Failed to snapshot refs for {id} in '{slug}'"))?;

    let row = collect_import_columns(doc_obj, def, id, locale)?;

    let placeholders: Vec<String> = (0..row.parent_cols.len())
        .map(|i| tx.placeholder(i + 1))
        .collect();

    let col_refs: Vec<&str> = row.parent_cols.iter().map(String::as_str).collect();

    let sql = tx.build_upsert(slug, &col_refs, &placeholders.join(", "), "id");

    tx.execute(&sql, &row.parent_vals)
        .with_context(|| format!("Failed to insert document {id} into '{slug}'"))?;

    if !row.join_data.is_empty() {
        query::save_join_table_data(tx, slug, &def.fields, id, &row.join_data, None)?;
    }

    query::ref_count::after_update(tx, slug, id, &def.fields, locale, &old_refs)
        .with_context(|| format!("Failed to update ref counts for {id} in '{slug}'"))?;

    Ok(())
}

/// Import collection data from JSON.
///
/// # Errors
///
/// Returns an error if config loading, file reading, JSON parsing, or any
/// per-document write fails.
#[cfg(not(tarpaulin_include))]
pub fn import(config_dir: &Path, file: &Path, collection_filter: Option<&str>) -> Result<()> {
    let cfg = CrapConfig::load(config_dir).context("Failed to load config")?;
    let (pool, registry) = load_config_and_sync(config_dir)?;

    let content =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;

    let export_file: ExportFile = serde_json::from_str(&content).context("Failed to parse JSON")?;

    // Refuse an export written by a newer format than this binary understands.
    if export_file.format_version > crate::commands::export::file::EXPORT_FORMAT_VERSION {
        bail!(
            "This export uses format version {} but this crap-cms only supports up to {}. \
             Upgrade crap-cms to import it.",
            export_file.format_version,
            crate::commands::export::file::EXPORT_FORMAT_VERSION
        );
    }

    let current = env!("CARGO_PKG_VERSION");
    if let Some(warning) =
        CrapConfig::check_version_against(Some(&export_file.crap_version), current)
    {
        cli::warning(&warning.replace("config requires", "export file was created with"));
    }

    let slugs: Vec<String> = if let Some(slug) = collection_filter {
        if !export_file.collections.contains_key(slug) {
            bail!("Collection '{slug}' not found in import file");
        }
        vec![slug.to_string()]
    } else {
        export_file.collections.keys().cloned().collect()
    };

    check_import_slugs(&registry, &slugs)?;

    let mut total_imported = 0usize;

    for slug in &slugs {
        let def = registry.get_collection(slug).ok_or_else(|| {
            anyhow!("Collection '{slug}' exists in import file but not in schema")
        })?;

        let docs_array = export_file
            .collections
            .get(slug)
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("Expected array for collection '{slug}'"))?;

        let mut conn = pool.get().context("Failed to get database connection")?;
        let tx = conn.transaction().context("Failed to begin transaction")?;

        for doc_val in docs_array {
            import_single_document(doc_val, slug, def, &tx, &cfg.locale)?;
            total_imported += 1;
        }

        tx.commit()
            .with_context(|| format!("Failed to commit import for '{slug}'"))?;

        cli::success(&format!(
            "Imported {} document(s) into '{}'",
            docs_array.len(),
            slug
        ));
    }

    cli::success(&format!("Total: {total_imported} document(s) imported"));

    Ok(())
}

/// Verify every collection in the import set exists in the registry BEFORE
/// any write. Each collection commits its own transaction, so resolving
/// slugs lazily inside the loop used to leave earlier collections imported
/// when a later slug was unknown — a partial import with no clean way back.
fn check_import_slugs(registry: &Registry, slugs: &[String]) -> Result<()> {
    let unknown: Vec<&str> = slugs
        .iter()
        .filter(|s| registry.get_collection(s).is_none())
        .map(String::as_str)
        .collect();

    if unknown.is_empty() {
        return Ok(());
    }

    bail!(
        "Collection(s) {} exist in the import file but not in the schema — nothing was imported",
        unknown
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::DatabaseConfig,
        core::field::RelationshipConfig,
        db::{migrate, pool},
    };

    fn setup_media_posts() -> (tempfile::TempDir, crate::db::DbPool, CollectionDefinition) {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Relationship)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        let posts_def = posts.clone();

        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

        let registry_shared = Registry::shared();
        {
            let mut reg = registry_shared.write().unwrap();
            reg.register_collection(media);
            reg.register_collection(posts);
        }
        let registry = (*Registry::snapshot(&registry_shared)).clone();
        migrate::sync_all(&db_pool, &registry, &LocaleConfig::default()).expect("sync");

        (tmp, db_pool, posts_def)
    }

    /// Regression: an unknown slug must be rejected before any collection is
    /// written (previously detected lazily, after earlier collections had
    /// already committed).
    #[test]
    fn unknown_import_slugs_rejected_up_front() {
        let shared = Registry::shared();
        shared
            .write()
            .unwrap()
            .register_collection(CollectionDefinition::new("posts"));
        let registry = (*Registry::snapshot(&shared)).clone();

        assert!(check_import_slugs(&registry, &["posts".to_string()]).is_ok());

        let err = check_import_slugs(
            &registry,
            &[
                "posts".to_string(),
                "ghosts".to_string(),
                "zombies".to_string(),
            ],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("'ghosts', 'zombies'"), "{err}");
        assert!(err.contains("nothing was imported"), "{err}");
    }

    /// Regression: imported relationships must adjust `_ref_count` — the
    /// raw upsert used to skip ref counting entirely, leaving imported
    /// references invisible to delete protection (and the backfill is
    /// version-gated, so it would never repair them).
    #[test]
    fn import_adjusts_ref_counts() {
        let (_tmp, db_pool, posts_def) = setup_media_posts();
        let lc = LocaleConfig::default();

        let mut conn = db_pool.get().unwrap();
        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();

        // New document referencing m1 → count goes to 1.
        let doc = json!({ "id": "p1", "image": "m1" });
        let tx = conn.transaction().unwrap();
        import_single_document(&doc, "posts", &posts_def, &tx, &lc).unwrap();
        tx.commit().unwrap();

        let conn2 = db_pool.get().unwrap();
        assert_eq!(
            query::ref_count::get_ref_count(&conn2, "media", "m1").unwrap(),
            Some(1)
        );
        drop(conn2);

        // Re-import the same document unchanged → count stays 1 (upsert
        // diffs old vs new refs, no double counting).
        let tx = conn.transaction().unwrap();
        import_single_document(&doc, "posts", &posts_def, &tx, &lc).unwrap();
        tx.commit().unwrap();

        let conn2 = db_pool.get().unwrap();
        assert_eq!(
            query::ref_count::get_ref_count(&conn2, "media", "m1").unwrap(),
            Some(1)
        );
        drop(conn2);

        // Re-import with the reference cleared → count drops to 0.
        let doc_cleared = json!({ "id": "p1", "image": null });
        let tx = conn.transaction().unwrap();
        import_single_document(&doc_cleared, "posts", &posts_def, &tx, &lc).unwrap();
        tx.commit().unwrap();

        let conn2 = db_pool.get().unwrap();
        assert_eq!(
            query::ref_count::get_ref_count(&conn2, "media", "m1").unwrap(),
            Some(0)
        );
    }

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
