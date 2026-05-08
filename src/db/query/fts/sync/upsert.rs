//! Per-document FTS upsert (delete + insert) with optional richtext extraction.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result};

use crate::core::{CollectionDefinition, Document, Registry};
use crate::db::query::fts::fields::{build_node_searchable_map, json_richtext_columns};
use crate::db::query::fts::prosemirror::extract_prosemirror_text_with_nodes;
use crate::db::query::fts::search::fts_table_name;
use crate::db::{DbConnection, DbValue};

use super::helpers::get_fts_table_columns;

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
pub(crate) fn fts_upsert_with_registry(
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
    let node_searchable = build_node_searchable_map(def, registry);
    let is_postgres = conn.kind() == "postgres";

    let logical_cols = resolve_logical_columns(doc, &fts_cols, is_postgres);
    let field_texts = extract_field_texts(doc, &logical_cols, &json_rt_cols, &node_searchable);

    if is_postgres {
        upsert_postgres(conn, &fts_table, doc, field_texts)?;
    } else {
        upsert_sqlite(conn, &fts_table, doc, &fts_cols, field_texts)?;
    }

    Ok(())
}

/// Determine which columns to index: Postgres uses all string fields, SQLite uses FTS columns.
fn resolve_logical_columns(doc: &Document, fts_cols: &[String], is_postgres: bool) -> Vec<String> {
    if is_postgres {
        doc.fields
            .iter()
            .filter(|(_, v)| v.is_string())
            .map(|(k, _)| k.clone())
            .collect()
    } else {
        fts_cols.to_vec()
    }
}

/// Extract text values for each logical column, handling richtext JSON extraction.
fn extract_field_texts(
    doc: &Document,
    logical_cols: &[String],
    json_rt_cols: &HashSet<String>,
    node_searchable: &HashMap<&str, Vec<&str>>,
) -> Vec<String> {
    logical_cols
        .iter()
        .map(|col_name| {
            let raw = doc
                .fields
                .get(col_name)
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let is_json_rt = json_rt_cols.contains(col_name)
                || col_name
                    .split("__")
                    .next()
                    .is_some_and(|base| json_rt_cols.contains(base));

            if is_json_rt && !raw.is_empty() {
                extract_prosemirror_text_with_nodes(raw, node_searchable)
            } else {
                raw.to_string()
            }
        })
        .collect()
}

/// Upsert into Postgres FTS (single tsvector column, ON CONFLICT).
fn upsert_postgres(
    conn: &dyn DbConnection,
    fts_table: &str,
    doc: &Document,
    field_texts: Vec<String>,
) -> Result<()> {
    let combined = field_texts.join(" ");
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    let sql = format!(
        "INSERT INTO {}(id, tsv) VALUES ({}, to_tsvector('simple', {})) \
         ON CONFLICT (id) DO UPDATE SET tsv = EXCLUDED.tsv",
        fts_table, p1, p2
    );

    conn.execute(
        &sql,
        &[DbValue::Text(doc.id.to_string()), DbValue::Text(combined)],
    )
    .with_context(|| format!("FTS upsert in {}", fts_table))?;

    Ok(())
}

/// Upsert into SQLite FTS5 (delete + insert, no ON CONFLICT support).
fn upsert_sqlite(
    conn: &dyn DbConnection,
    fts_table: &str,
    doc: &Document,
    fts_cols: &[String],
    field_texts: Vec<String>,
) -> Result<()> {
    conn.execute(
        &format!(
            "DELETE FROM {} WHERE id = {}",
            fts_table,
            conn.placeholder(1)
        ),
        &[DbValue::Text(doc.id.to_string())],
    )
    .with_context(|| format!("FTS delete before upsert in {}", fts_table))?;

    let mut values: Vec<DbValue> = vec![DbValue::Text(doc.id.to_string())];

    for text in field_texts {
        values.push(DbValue::Text(text));
    }

    let placeholders: Vec<String> = (1..=values.len()).map(|i| conn.placeholder(i)).collect();
    let sql = format!(
        "INSERT INTO {}(id, {}) VALUES ({})",
        fts_table,
        fts_cols.join(", "),
        placeholders.join(", ")
    );

    conn.execute(&sql, &values)
        .with_context(|| format!("FTS upsert in {}", fts_table))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::{CrapConfig, LocaleConfig};
    use crate::core::field::*;
    use crate::core::{Registry, richtext::RichtextNodeDef};
    use crate::db::query::fts::search::fts_search;
    use crate::db::query::fts::sync::sync_fts_table;
    use crate::db::query::fts::sync::test_helpers::*;
    use crate::db::{BoxedConnection, pool};
    use tempfile::TempDir;

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
    fn upsert_noop_no_fts_table() {
        let (_dir, conn) = setup_db();
        let doc = Document::new("1".to_string());
        fts_upsert(&conn, "posts", &doc, None).unwrap();
    }

    #[test]
    fn upsert_with_locale_columns() {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn: BoxedConnection = p.get().unwrap();
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
