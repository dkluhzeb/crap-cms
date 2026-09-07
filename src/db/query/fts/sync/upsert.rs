//! Per-document FTS upsert (delete + insert) with optional richtext extraction.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result};

use crate::config::LocaleConfig;
use crate::core::{CollectionDefinition, Registry};
use crate::db::query::fts::extract::extract_prosemirror_text_with_nodes;
use crate::db::query::fts::fields::{
    build_node_searchable_map, get_fts_columns, is_json_richtext_column, json_richtext_columns,
};
use crate::db::query::fts::search::{fts_table_name, table_exists};
use crate::db::query::fts::sync::fts_delete;
use crate::db::query::helpers::{placeholder_list, quote_ident};
use crate::db::{DbConnection, DbRow, DbValue};

use super::helpers::pg_tsvector;

/// Insert or update a document in the FTS index.
///
/// Reads the indexed columns straight from the document's row — the same
/// column set the startup rebuild indexes (`get_fts_columns`, so both
/// backends index exactly the searchable fields, per locale). Callers only
/// pass the id: the index never depends on the shape of an in-memory
/// `Document`, which a locale-aware re-read aliases (`title__en AS title`)
/// and which would otherwise index every column as empty.
///
/// No-op if the FTS table doesn't exist; a vanished row drops its index
/// entry.
///
/// # Errors
///
/// Returns a backend error if the row read, DELETE, or INSERT fails.
pub fn fts_upsert(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    fts_upsert_with_registry(conn, slug, id, def, locale_config, None)
}

/// Like `fts_upsert`, but accepts an optional registry for resolving custom
/// richtext node searchable attrs.
pub(crate) fn fts_upsert_with_registry(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
    registry: Option<&Registry>,
) -> Result<()> {
    let fts_table = fts_table_name(slug);

    if !table_exists(conn, &fts_table) {
        return Ok(());
    }

    let fts_cols = get_fts_columns(def, locale_config)?;
    if fts_cols.is_empty() {
        return Ok(());
    }

    let Some(row) = read_indexed_row(conn, slug, id, &fts_cols)? else {
        return fts_delete(conn, slug, id);
    };

    let json_rt_cols = json_richtext_columns(def);
    let node_searchable = build_node_searchable_map(Some(def), registry);
    let field_texts = extract_field_texts(&row, &fts_cols, &json_rt_cols, &node_searchable);

    if conn.is_postgres() {
        upsert_postgres(conn, &fts_table, id, &field_texts)
    } else {
        upsert_sqlite(conn, &fts_table, id, &fts_cols, field_texts)
    }
}

/// Select the indexed columns of one row, in `fts_cols` order, NULLs as `''`.
fn read_indexed_row(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    fts_cols: &[String],
) -> Result<Option<DbRow>> {
    let select: Vec<String> = fts_cols
        .iter()
        .map(|c| format!("COALESCE({}, '')", quote_ident(c)))
        .collect();
    let sql = format!(
        "SELECT {} FROM \"{slug}\" WHERE id = {}",
        select.join(", "),
        conn.placeholder(1)
    );

    conn.query_one(&sql, &[DbValue::Text(id.to_string())])
        .with_context(|| format!("FTS row read from {slug}"))
}

/// Extract the text of each indexed column, expanding JSON richtext to
/// plain text.
fn extract_field_texts(
    row: &DbRow,
    fts_cols: &[String],
    json_rt_cols: &HashSet<String>,
    node_searchable: &HashMap<&str, Vec<&str>>,
) -> Vec<String> {
    fts_cols
        .iter()
        .enumerate()
        .map(|(i, col_name)| {
            let raw = row.text_at(i).unwrap_or("");

            let is_json_rt = is_json_richtext_column(col_name, json_rt_cols);

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
    id: &str,
    field_texts: &[String],
) -> Result<()> {
    let combined = field_texts.join(" ");
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    let sql = format!(
        "INSERT INTO {fts_table}(id, tsv) VALUES ({p1}, {}) \
         ON CONFLICT (id) DO UPDATE SET tsv = EXCLUDED.tsv",
        pg_tsvector(&p2)
    );

    conn.execute(
        &sql,
        &[DbValue::Text(id.to_string()), DbValue::Text(combined)],
    )
    .with_context(|| format!("FTS upsert in {fts_table}"))?;

    Ok(())
}

/// Upsert into `SQLite` FTS5 (delete + insert, no ON CONFLICT support).
fn upsert_sqlite(
    conn: &dyn DbConnection,
    fts_table: &str,
    id: &str,
    fts_cols: &[String],
    field_texts: Vec<String>,
) -> Result<()> {
    conn.execute(
        &format!(
            "DELETE FROM {} WHERE id = {}",
            fts_table,
            conn.placeholder(1)
        ),
        &[DbValue::Text(id.to_string())],
    )
    .with_context(|| format!("FTS delete before upsert in {fts_table}"))?;

    let mut values: Vec<DbValue> = vec![DbValue::Text(id.to_string())];

    for text in field_texts {
        values.push(DbValue::Text(text));
    }

    let placeholders = placeholder_list(conn, values.len());
    let quoted_cols = fts_cols
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("INSERT INTO {fts_table}(id, {quoted_cols}) VALUES ({placeholders})");

    conn.execute(&sql, &values)
        .with_context(|| format!("FTS upsert in {fts_table}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrapConfig, LocaleConfig};
    use crate::core::field::*;
    use crate::core::{Registry, richtext::RichtextNodeDef};
    use crate::db::query::fts::sync::sync_fts_table;
    use crate::db::query::fts::sync::test_helpers::*;
    use crate::db::{BoxedConnection, pool};
    use tempfile::TempDir;

    fn no_locale() -> LocaleConfig {
        LocaleConfig::default()
    }

    #[test]
    fn upsert_and_search() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &no_locale()).unwrap();

        insert_post(&conn, "new1", "Unique Title", "Some content");
        fts_upsert(&conn, "posts", "new1", &def, &no_locale()).unwrap();

        let results = fts_match_ids(&conn, "posts", "Unique", 10).unwrap();
        assert_eq!(results, vec!["new1"]);
        let results = fts_match_ids(&conn, "posts", "content", 10).unwrap();
        assert_eq!(results, vec!["new1"]);
    }

    #[test]
    fn upsert_updates_existing() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "1", "Old Title", "");
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &no_locale()).unwrap();

        conn.execute(
            "UPDATE posts SET title = ?1 WHERE id = ?2",
            &[DbValue::Text("New Title".into()), DbValue::Text("1".into())],
        )
        .unwrap();
        fts_upsert(&conn, "posts", "1", &def, &no_locale()).unwrap();

        let old_results = fts_match_ids(&conn, "posts", "Old", 10).unwrap();
        assert!(old_results.is_empty());

        let new_results = fts_match_ids(&conn, "posts", "New", 10).unwrap();
        assert_eq!(new_results, vec!["1"]);
    }

    #[test]
    fn upsert_noop_no_fts_table() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);
        fts_upsert(&conn, "posts", "1", &def, &no_locale()).unwrap();
    }

    /// A vanished row drops its index entry instead of indexing empty text.
    #[test]
    fn upsert_of_a_missing_row_removes_the_index_entry() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "gone", "Ephemeral", "");
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &no_locale()).unwrap();
        assert_eq!(
            fts_match_ids(&conn, "posts", "Ephemeral", 10).unwrap(),
            vec!["gone"]
        );

        conn.execute("DELETE FROM posts WHERE id = 'gone'", &[])
            .unwrap();
        fts_upsert(&conn, "posts", "gone", &def, &no_locale()).unwrap();

        assert!(
            fts_match_ids(&conn, "posts", "Ephemeral", 10)
                .unwrap()
                .is_empty()
        );
    }

    /// The index reads the per-locale columns from the row itself, so it is
    /// independent of how a caller's re-read aliased them.
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

        conn.execute(
            "INSERT INTO posts (id, title__en, title__de) VALUES ('doc1', 'English Title', 'Deutscher Titel')",
            &[],
        )
        .unwrap();
        fts_upsert(&conn, "posts", "doc1", &def, &locale).unwrap();

        let en_results = fts_match_ids(&conn, "posts", "English", 10).unwrap();
        assert_eq!(en_results, vec!["doc1"]);

        let de_results = fts_match_ids(&conn, "posts", "Deutscher", 10).unwrap();
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
        sync_fts_table(&conn, "posts", &def, &no_locale()).unwrap();

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

        fts_upsert(&conn, "posts", "1", &def, &no_locale()).unwrap();

        let results = fts_match_ids(&conn, "posts", "Searchable", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        let results = fts_match_ids(&conn, "posts", "paragraph", 10).unwrap();
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

        sync_fts_table(&conn, "posts", &def, &no_locale()).unwrap();

        let pm_json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello"}]},{"type":"cta","attrs":{"button_text":"Click Here","url":"/go"}}]}"#;
        conn.execute(
            "INSERT INTO posts (id, title, content, created_at, updated_at) VALUES (?1, ?2, ?3, datetime('now'), datetime('now'))",
            &[
                DbValue::Text("rg1".into()),
                DbValue::Text("Registry Test".into()),
                DbValue::Text(pm_json.into()),
            ],
        )
        .unwrap();

        fts_upsert_with_registry(&conn, "posts", "rg1", &def, &no_locale(), Some(&registry))
            .unwrap();

        let results = fts_match_ids(&conn, "posts", "Hello", 10).unwrap();
        assert_eq!(results, vec!["rg1"]);

        let results = fts_match_ids(&conn, "posts", "Click", 10).unwrap();
        assert_eq!(results, vec!["rg1"]);

        let results = fts_match_ids(&conn, "posts", "go", 10).unwrap();
        assert!(results.is_empty() || !results.contains(&"rg1".to_string()));
    }

    #[test]
    fn fts_upsert_with_registry_noop_no_fts_table() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);
        let registry = Registry::new();
        let result =
            fts_upsert_with_registry(&conn, "posts", "1", &def, &no_locale(), Some(&registry));
        assert!(result.is_ok(), "should be a no-op when no FTS table exists");
    }

    #[test]
    fn fts_upsert_with_registry_nil_inputs_use_plain_extraction() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &no_locale()).unwrap();

        insert_post(&conn, "plain1", "Plain text", "");
        fts_upsert_with_registry(&conn, "posts", "plain1", &def, &no_locale(), None).unwrap();

        let results = fts_match_ids(&conn, "posts", "Plain", 10).unwrap();
        assert_eq!(results, vec!["plain1"]);
    }

    /// A non-text column value (NULL here) indexes as empty, never as an error.
    #[test]
    fn fts_upsert_null_column_indexes_as_empty() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &no_locale()).unwrap();

        conn.execute(
            "INSERT INTO posts (id, title, body, created_at, updated_at) VALUES ('n1', 'Only', NULL, '', '')",
            &[],
        )
        .unwrap();
        fts_upsert(&conn, "posts", "n1", &def, &no_locale()).unwrap();

        assert_eq!(
            fts_match_ids(&conn, "posts", "Only", 10).unwrap(),
            vec!["n1"]
        );
    }
}
