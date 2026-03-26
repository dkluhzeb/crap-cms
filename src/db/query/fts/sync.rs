//! FTS5 index synchronization, upsert, and delete operations.

use anyhow::{Context as _, Result, bail};

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, Registry},
    db::{DbConnection, DbValue},
};

use super::{
    fields::{build_node_searchable_map, get_fts_columns, json_richtext_columns},
    prosemirror::{extract_prosemirror_text, extract_prosemirror_text_with_nodes},
    search::{fts_table_name, table_exists},
};

/// Get column names from the FTS table (excludes `id`).
///
/// Returns `None` if the FTS table doesn't exist or has no columns.
fn get_fts_table_columns(conn: &dyn DbConnection, fts_table: &str) -> Option<Vec<String>> {
    if !table_exists(conn, fts_table) {
        return None;
    }

    // Use PRAGMA table_info (not table_xinfo) — table_xinfo includes hidden
    // virtual columns like the table name and rank which aren't real data columns.
    let rows = conn
        .query_all(&format!("PRAGMA table_info({})", fts_table), &[])
        .ok()?;

    let cols: Vec<String> = rows
        .into_iter()
        .filter_map(|row| {
            if let Some(DbValue::Text(name)) = row.get_value(1)
                && name != "id"
            {
                return Some(name.clone());
            }
            None
        })
        .collect();

    if cols.is_empty() { None } else { Some(cols) }
}

/// Drop and recreate the FTS5 virtual table, then bulk-populate from the main table.
///
/// Called during migration (startup). Always rebuilds fresh — avoids drift detection.
/// If there are no indexable columns, drops the FTS table if it exists.
pub fn sync_fts_table(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let fts_table = fts_table_name(slug);
    let fts_fields = get_fts_columns(def, locale_config);

    // Always drop existing FTS table first
    conn.execute_batch(&format!("DROP TABLE IF EXISTS {}", fts_table))
        .with_context(|| format!("Failed to drop FTS table {}", fts_table))?;

    if fts_fields.is_empty() {
        return Ok(());
    }

    // Validate field names (defense against injection — they come from Lua config)
    for f in &fts_fields {
        if !crate::db::query::is_valid_identifier(f) {
            bail!(
                "Invalid FTS field name '{}': must be alphanumeric/underscore",
                f
            );
        }
    }

    // Create FTS5 virtual table
    let field_list = fts_fields.join(", ");
    let create_sql = format!(
        "CREATE VIRTUAL TABLE {} USING fts5(id UNINDEXED, {})",
        fts_table, field_list
    );
    conn.execute_batch(&create_sql)
        .with_context(|| format!("Failed to create FTS table {}", fts_table))?;

    // Bulk populate from main table
    let json_rt_cols = json_richtext_columns(def);

    if json_rt_cols.is_empty() {
        bulk_populate_fast(conn, slug, &fts_table, &fts_fields, &field_list)
    } else {
        bulk_populate_slow(
            conn,
            slug,
            &fts_table,
            &fts_fields,
            &field_list,
            &json_rt_cols,
        )
    }
}

/// Fast path: no JSON richtext fields, pure SQL bulk insert.
fn bulk_populate_fast(
    conn: &dyn DbConnection,
    slug: &str,
    fts_table: &str,
    fts_fields: &[String],
    field_list: &str,
) -> Result<()> {
    let select_fields: Vec<String> = fts_fields
        .iter()
        .map(|f| format!("COALESCE({}, '')", f))
        .collect();
    let insert_sql = format!(
        "INSERT INTO {}(id, {}) SELECT id, {} FROM {}",
        fts_table,
        field_list,
        select_fields.join(", "),
        slug
    );
    conn.execute_batch(&insert_sql)
        .with_context(|| format!("Failed to populate FTS table {}", fts_table))?;
    Ok(())
}

/// Slow path: read rows and extract plain text from JSON richtext fields.
fn bulk_populate_slow(
    conn: &dyn DbConnection,
    slug: &str,
    fts_table: &str,
    fts_fields: &[String],
    field_list: &str,
    json_rt_cols: &std::collections::HashSet<String>,
) -> Result<()> {
    let select_fields: Vec<String> = fts_fields
        .iter()
        .map(|f| format!("COALESCE({}, '')", f))
        .collect();
    let select_sql = format!("SELECT id, {} FROM {}", select_fields.join(", "), slug);

    let db_rows = conn
        .query_all(&select_sql, &[])
        .with_context(|| format!("Failed to query {} for FTS population", slug))?;

    let placeholders: Vec<String> = (1..=fts_fields.len() + 1)
        .map(|i| conn.placeholder(i))
        .collect();
    let insert_sql = format!(
        "INSERT INTO {}(id, {}) VALUES ({})",
        fts_table,
        field_list,
        placeholders.join(", ")
    );

    for row in db_rows {
        let id = match row.get_value(0) {
            Some(DbValue::Text(s)) => s.clone(),
            _ => continue,
        };

        let mut params: Vec<DbValue> = vec![DbValue::Text(id)];

        for (i, col_name) in fts_fields.iter().enumerate() {
            let raw = match row.get_value(i + 1) {
                Some(DbValue::Text(s)) => s.clone(),
                _ => String::new(),
            };
            let is_json_rt = json_rt_cols.contains(col_name)
                || col_name
                    .split("__")
                    .next()
                    .map(|base| json_rt_cols.contains(base))
                    .unwrap_or(false);
            let text = if is_json_rt && !raw.is_empty() {
                extract_prosemirror_text(&raw)
            } else {
                raw
            };
            params.push(DbValue::Text(text));
        }

        conn.execute(&insert_sql, &params)
            .with_context(|| format!("FTS bulk insert in {}", fts_table))?;
    }
    Ok(())
}

/// Insert or update a document in the FTS index.
///
/// Deletes the existing row (if any) then inserts fresh data.
/// No-op if the FTS table doesn't exist. Column list is read from the FTS table
/// at runtime, so callers don't need locale awareness.
///
/// If `def` is provided, JSON-format richtext fields are extracted to plain text.
pub fn fts_upsert(
    conn: &dyn DbConnection,
    slug: &str,
    doc: &Document,
    def: Option<&CollectionDefinition>,
) -> Result<()> {
    fts_upsert_with_registry(conn, slug, doc, def, None)
}

/// Like `fts_upsert`, but accepts an optional registry for resolving custom
/// richtext node searchable attrs.
pub fn fts_upsert_with_registry(
    conn: &dyn DbConnection,
    slug: &str,
    doc: &Document,
    def: Option<&CollectionDefinition>,
    registry: Option<&Registry>,
) -> Result<()> {
    let fts_table = fts_table_name(slug);

    let fts_cols = match get_fts_table_columns(conn, &fts_table) {
        Some(cols) => cols,
        None => return Ok(()),
    };

    let json_rt_cols = def.map(json_richtext_columns).unwrap_or_default();

    // Build searchable attrs map for custom richtext nodes
    let node_searchable = build_node_searchable_map(def, registry);

    // Delete existing row
    conn.execute(
        &format!(
            "DELETE FROM {} WHERE id = {}",
            fts_table,
            conn.placeholder(1)
        ),
        &[DbValue::Text(doc.id.to_string())],
    )
    .with_context(|| format!("FTS delete before upsert in {}", fts_table))?;

    // Insert new row
    let mut values: Vec<DbValue> = vec![DbValue::Text(doc.id.to_string())];

    for col_name in &fts_cols {
        let raw = doc
            .fields
            .get(col_name)
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Check if this column is a JSON-format richtext field
        // (either exact match or the base name before "__locale" suffix)
        let is_json_rt = json_rt_cols.contains(col_name)
            || col_name
                .split("__")
                .next()
                .map(|base| json_rt_cols.contains(base))
                .unwrap_or(false);

        let text = if is_json_rt && !raw.is_empty() {
            if node_searchable.is_empty() {
                extract_prosemirror_text(raw)
            } else {
                extract_prosemirror_text_with_nodes(raw, &node_searchable)
            }
        } else {
            raw.to_string()
        };
        values.push(DbValue::Text(text));
    }

    let placeholders: Vec<String> = (1..=values.len()).map(|i| conn.placeholder(i)).collect();
    let field_list: String = fts_cols.join(", ");
    let sql = format!(
        "INSERT INTO {}(id, {}) VALUES ({})",
        fts_table,
        field_list,
        placeholders.join(", ")
    );

    conn.execute(&sql, &values)
        .with_context(|| format!("FTS upsert in {}", fts_table))?;

    Ok(())
}

/// Delete a document from the FTS index.
///
/// No-op if the FTS table doesn't exist.
pub fn fts_delete(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<()> {
    let fts_table = fts_table_name(slug);

    if !table_exists(conn, &fts_table) {
        return Ok(());
    }

    conn.execute(
        &format!(
            "DELETE FROM {} WHERE id = {}",
            fts_table,
            conn.placeholder(1)
        ),
        &[DbValue::Text(id.to_string())],
    )
    .with_context(|| format!("FTS delete in {}", fts_table))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::{CrapConfig, LocaleConfig};
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::query::fts::search::fts_search;
    use crate::db::{BoxedConnection, DbValue, pool};
    use tempfile::TempDir;

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn simple_def(fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = fields;
        def
    }

    fn setup_db() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                body TEXT,
                status TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();
        (dir, conn)
    }

    fn insert_post(conn: &dyn DbConnection, id: &str, title: &str, body: &str) {
        conn.execute(
            "INSERT INTO posts (id, title, body, created_at, updated_at) VALUES (?1, ?2, ?3, datetime('now'), datetime('now'))",
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(title.to_string()),
                DbValue::Text(body.to_string()),
            ],
        ).unwrap();
    }

    fn locale_config_en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: false,
        }
    }

    fn localized_text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .localized(true)
            .build()
    }

    // ── sync_fts_table ──────────────────────────────────────────────────

    #[test]
    fn sync_creates_and_populates() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "1", "Hello World", "Body text");
        insert_post(&conn, "2", "Rust FTS", "Full text search");

        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let exists = conn
            .query_one(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='_fts_posts'",
                &[],
            )
            .unwrap()
            .and_then(|row| {
                if let Some(DbValue::Integer(n)) = row.get_value(0) {
                    Some(*n != 0)
                } else {
                    None
                }
            })
            .unwrap_or(false);
        assert!(exists);

        let count = conn
            .query_one("SELECT COUNT(*) FROM _fts_posts", &[])
            .unwrap()
            .and_then(|row| {
                if let Some(DbValue::Integer(n)) = row.get_value(0) {
                    Some(*n)
                } else {
                    None
                }
            })
            .unwrap_or(0);
        assert_eq!(count, 2);
    }

    #[test]
    fn sync_drops_when_no_fields() {
        let (_dir, conn) = setup_db();
        conn.execute_batch("CREATE VIRTUAL TABLE _fts_posts USING fts5(id UNINDEXED, title)")
            .unwrap();

        let def = simple_def(vec![
            FieldDefinition::builder("count", FieldType::Number).build(),
        ]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let exists = conn
            .query_one(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='_fts_posts'",
                &[],
            )
            .unwrap()
            .and_then(|row| {
                if let Some(DbValue::Integer(n)) = row.get_value(0) {
                    Some(*n != 0)
                } else {
                    None
                }
            })
            .unwrap_or(false);
        assert!(!exists);
    }

    #[test]
    fn sync_rebuilds_on_field_change() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "1", "Hello", "World");

        let mut def1 = simple_def(vec![text_field("title"), text_field("body")]);
        def1.admin.list_searchable_fields = vec!["title".into()];
        sync_fts_table(&conn, "posts", &def1, &LocaleConfig::default()).unwrap();

        let results = fts_search(&conn, "posts", "Hello", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        let mut def2 = simple_def(vec![text_field("title"), text_field("body")]);
        def2.admin.list_searchable_fields = vec!["title".into(), "body".into()];
        sync_fts_table(&conn, "posts", &def2, &LocaleConfig::default()).unwrap();

        let results = fts_search(&conn, "posts", "World", 10).unwrap();
        assert_eq!(results, vec!["1"]);
    }

    #[test]
    fn sync_fts_table_rejects_invalid_field_names() {
        let (_dir, conn) = setup_db();
        // Include the bad-named fields in the definition so they aren't filtered
        // out as non-existent — tests that the SQL validation layer catches them.
        let mut def = simple_def(vec![text_field("title"), text_field("has space")]);
        def.admin.list_searchable_fields = vec!["title".into(), "has space".into()];
        let result = sync_fts_table(&conn, "posts", &def, &LocaleConfig::default());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid FTS field name"),
            "Error should mention invalid field: {}",
            err_msg
        );
    }

    #[test]
    fn sync_fts_table_rejects_sql_injection_field_names() {
        let (_dir, conn) = setup_db();
        // Include the injection-named field in the definition so it reaches the validator.
        let mut def = simple_def(vec![
            text_field("title"),
            text_field("title; DROP TABLE posts"),
        ]);
        def.admin.list_searchable_fields = vec!["title; DROP TABLE posts".into()];
        let result = sync_fts_table(&conn, "posts", &def, &LocaleConfig::default());
        assert!(result.is_err());
    }

    #[test]
    fn sync_fts_table_creates_locale_columns() {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title__en TEXT,
                title__de TEXT,
                body TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts (id, title__en, title__de, body) VALUES (?1, ?2, ?3, ?4)",
            &[
                DbValue::Text("1".into()),
                DbValue::Text("Hello".into()),
                DbValue::Text("Hallo".into()),
                DbValue::Text("Content".into()),
            ],
        )
        .unwrap();

        let def = simple_def(vec![localized_text_field("title"), text_field("body")]);
        let locale = locale_config_en_de();
        sync_fts_table(&conn, "posts", &def, &locale).unwrap();

        let cols = get_fts_table_columns(&conn, "_fts_posts").unwrap();
        assert!(cols.contains(&"title__en".to_string()));
        assert!(cols.contains(&"title__de".to_string()));
        assert!(cols.contains(&"body".to_string()));
        assert_eq!(cols.len(), 3);

        let results = fts_search(&conn, "posts", "Hello", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        let results = fts_search(&conn, "posts", "Hallo", 10).unwrap();
        assert_eq!(results, vec!["1"]);
    }

    #[test]
    fn sync_fts_table_slow_path_json_richtext_bulk_populate() {
        let (_dir, conn) = setup_db();
        conn.execute_batch("ALTER TABLE posts ADD COLUMN content TEXT")
            .unwrap();

        let pm_json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Extracted content"}]}]}"#;
        conn.execute(
            "INSERT INTO posts (id, title, content, created_at, updated_at) VALUES (?1, ?2, ?3, datetime('now'), datetime('now'))",
            &[
                DbValue::Text("1".into()),
                DbValue::Text("Test".into()),
                DbValue::Text(pm_json.into()),
            ],
        )
        .unwrap();

        let mut def = simple_def(vec![
            text_field("title"),
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .richtext_format("json".to_string())
                        .build(),
                )
                .build(),
        ]);
        def.admin.list_searchable_fields = vec!["title".into(), "content".into()];

        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let results = fts_search(&conn, "posts", "Extracted", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        let results = fts_search(&conn, "posts", "paragraph", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn sync_fts_table_slow_path_locale_richtext() {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        let pm_json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello locale"}]}]}"#;
        conn.execute_batch(&format!(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, content__en TEXT, content__de TEXT, created_at TEXT, updated_at TEXT);
             INSERT INTO posts (id, content__en, content__de) VALUES ('1', '{pm}', '');"
            , pm = pm_json.replace('\'', "''")
        ))
        .unwrap();

        let mut def = simple_def(vec![
            FieldDefinition::builder("content", FieldType::Richtext)
                .localized(true)
                .admin(FieldAdmin::builder().richtext_format("json").build())
                .build(),
        ]);
        def.admin.list_searchable_fields = vec!["content".into()];

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: false,
        };

        sync_fts_table(&conn, "posts", &def, &locale_config).unwrap();

        let results = fts_search(&conn, "posts", "locale", 10).unwrap();
        assert_eq!(results, vec!["1"]);
    }

    // ── fts_upsert / fts_delete ─────────────────────────────────────────

    #[test]
    fn upsert_and_search() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        insert_post(&conn, "new1", "Unique Title", "Some content");
        let mut doc = Document::new("new1".to_string());
        doc.fields.insert("title".into(), json!("Unique Title"));
        doc.fields.insert("body".into(), json!("Some content"));
        fts_upsert(&conn, "posts", &doc, None).unwrap();

        let results = fts_search(&conn, "posts", "Unique", 10).unwrap();
        assert_eq!(results, vec!["new1"]);
    }

    #[test]
    fn upsert_updates_existing() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "1", "Old Title", "");
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let mut doc = Document::new("1".to_string());
        doc.fields.insert("title".into(), json!("New Title"));
        doc.fields.insert("body".into(), json!(""));
        fts_upsert(&conn, "posts", &doc, None).unwrap();

        let old_results = fts_search(&conn, "posts", "Old", 10).unwrap();
        assert!(old_results.is_empty());

        let new_results = fts_search(&conn, "posts", "New", 10).unwrap();
        assert_eq!(new_results, vec!["1"]);
    }

    #[test]
    fn delete_removes_from_index() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "1", "Searchable", "");
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        assert_eq!(
            fts_search(&conn, "posts", "Searchable", 10).unwrap().len(),
            1
        );

        fts_delete(&conn, "posts", "1").unwrap();
        assert!(
            fts_search(&conn, "posts", "Searchable", 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn upsert_noop_no_fts_table() {
        let (_dir, conn) = setup_db();
        let doc = Document::new("1".to_string());
        fts_upsert(&conn, "posts", &doc, None).unwrap();
    }

    #[test]
    fn delete_noop_no_fts_table() {
        let (_dir, conn) = setup_db();
        fts_delete(&conn, "posts", "1").unwrap();
    }

    #[test]
    fn upsert_with_locale_columns() {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title__en TEXT,
                title__de TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let def = simple_def(vec![localized_text_field("title")]);
        let locale = locale_config_en_de();
        sync_fts_table(&conn, "posts", &def, &locale).unwrap();

        let mut doc = Document::new("doc1".to_string());
        doc.fields
            .insert("title__en".into(), json!("English Title"));
        doc.fields
            .insert("title__de".into(), json!("Deutscher Titel"));
        fts_upsert(&conn, "posts", &doc, None).unwrap();

        let en_results = fts_search(&conn, "posts", "English", 10).unwrap();
        assert_eq!(en_results, vec!["doc1"]);

        let de_results = fts_search(&conn, "posts", "Deutscher", 10).unwrap();
        assert_eq!(de_results, vec!["doc1"]);
    }

    #[test]
    fn fts_upsert_json_richtext() {
        let (_dir, conn) = setup_db();
        conn.execute_batch("ALTER TABLE posts ADD COLUMN content TEXT")
            .unwrap();

        let mut def = simple_def(vec![
            text_field("title"),
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(FieldAdmin::builder().richtext_format("json").build())
                .build(),
        ]);
        def.admin.list_searchable_fields = vec!["title".into(), "content".into()];
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let pm_json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Searchable text inside JSON"}]}]}"#;
        conn.execute(
            "INSERT INTO posts (id, title, content, created_at, updated_at) VALUES (?1, ?2, ?3, datetime('now'), datetime('now'))",
            &[
                DbValue::Text("1".into()),
                DbValue::Text("Test".into()),
                DbValue::Text(pm_json.into()),
            ],
        )
        .unwrap();

        let mut doc = Document::new("1".to_string());
        doc.fields.insert("title".into(), json!("Test"));
        doc.fields.insert("content".into(), json!(pm_json));
        fts_upsert(&conn, "posts", &doc, Some(&def)).unwrap();

        let results = fts_search(&conn, "posts", "Searchable", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        let results = fts_search(&conn, "posts", "paragraph", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn fts_upsert_with_registry_extracts_node_attrs() {
        use crate::core::{Registry, richtext::RichtextNodeDef};

        let (_dir, conn) = setup_db();
        conn.execute_batch("ALTER TABLE posts ADD COLUMN content TEXT")
            .unwrap();

        let mut def = simple_def(vec![
            text_field("title"),
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .richtext_format("json")
                        .nodes(vec!["cta".to_string()])
                        .build(),
                )
                .build(),
        ]);
        def.admin.list_searchable_fields = vec!["title".into(), "content".into()];

        let mut registry = Registry::new();
        registry.register_richtext_node(RichtextNodeDef {
            name: "cta".to_string(),
            label: "Call to Action".to_string(),
            inline: false,
            attrs: vec![],
            searchable_attrs: vec!["button_text".to_string()],
            has_render: false,
        });

        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let pm_json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello"}]},{"type":"cta","attrs":{"button_text":"Click Here","url":"/go"}}]}"#;

        let mut doc = Document::new("rg1".to_string());
        doc.fields.insert("title".into(), json!("Registry Test"));
        doc.fields.insert("content".into(), json!(pm_json));

        fts_upsert_with_registry(&conn, "posts", &doc, Some(&def), Some(&registry)).unwrap();

        let results = fts_search(&conn, "posts", "Hello", 10).unwrap();
        assert_eq!(results, vec!["rg1"]);

        let results = fts_search(&conn, "posts", "Click", 10).unwrap();
        assert_eq!(results, vec!["rg1"]);

        let results = fts_search(&conn, "posts", "go", 10).unwrap();
        assert!(results.is_empty() || !results.contains(&"rg1".to_string()));
    }

    #[test]
    fn fts_upsert_with_registry_noop_no_fts_table() {
        use crate::core::Registry;

        let (_dir, conn) = setup_db();
        let doc = Document::new("1".to_string());
        let registry = Registry::new();
        let result = fts_upsert_with_registry(&conn, "posts", &doc, None, Some(&registry));
        assert!(result.is_ok(), "should be a no-op when no FTS table exists");
    }

    #[test]
    fn fts_upsert_with_registry_nil_inputs_use_plain_extraction() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let mut doc = Document::new("plain1".to_string());
        doc.fields.insert("title".into(), json!("Plain text"));

        fts_upsert_with_registry(&conn, "posts", &doc, None, None).unwrap();

        let results = fts_search(&conn, "posts", "Plain", 10).unwrap();
        assert_eq!(results, vec!["plain1"]);
    }

    #[test]
    fn get_fts_table_columns_nonexistent_returns_none() {
        let (_dir, conn) = setup_db();
        let doc = Document::new("x".to_string());
        fts_upsert(&conn, "posts", &doc, None).unwrap();
    }

    #[test]
    fn fts_upsert_field_with_non_string_value_uses_empty() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let mut doc = Document::new("obj1".to_string());
        doc.fields
            .insert("title".into(), json!({"nested": "object"}));
        fts_upsert(&conn, "posts", &doc, None).unwrap();

        let results = fts_search(&conn, "posts", "nested", 10).unwrap();
        assert!(results.is_empty());
    }
}
