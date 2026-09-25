//! Per-document FTS upsert (delete + insert), reading rich text columns as text.

use anyhow::{Context as _, Result};

use crate::db::query::fts::index::{ColumnText, FtsIndex};
use crate::db::query::fts::layout::{FtsColumn, fts_columns, pg_vectors};
use crate::db::query::fts::search::{fts_table_name, table_exists};
use crate::db::query::fts::sync::fts_delete;
use crate::db::query::helpers::{placeholder_list, quote_ident};
use crate::db::{DbConnection, DbRow, DbValue};

use super::helpers::{column_texts, pg_tsvector, pg_vector_texts};

/// Insert or update document `id` in `index`.
///
/// Reads the indexed columns straight from the document's row — the same
/// columns, through the same read expressions, the startup rebuild indexes
/// (`fts_columns`, so both backends index exactly the searchable fields, per
/// locale and through the locale fallback). Callers only pass the id: the
/// index never depends on the shape of an in-memory `Document`, which a
/// locale-aware re-read aliases (`title__en AS title`) and which would
/// otherwise index every column as empty.
///
/// No-op if the FTS table doesn't exist; a vanished row drops its index
/// entry.
///
/// # Errors
///
/// Returns a backend error if the row read, DELETE, or INSERT fails.
pub fn fts_upsert(conn: &dyn DbConnection, index: &FtsIndex<'_>, id: &str) -> Result<()> {
    let fts_table = fts_table_name(index.slug);

    if !table_exists(conn, &fts_table) {
        return Ok(());
    }

    let columns = fts_columns(index.def, index.locale_config)?;
    if columns.is_empty() {
        return Ok(());
    }

    let Some(row) = read_indexed_row(conn, index.slug, id, &columns)? else {
        return fts_delete(conn, index.slug, id);
    };

    let texts = column_texts(&columns, &ColumnText::new(index), |i| row.text_at(i));
    let written = written_columns(conn, index, columns, texts)?;

    if conn.is_postgres() {
        upsert_postgres(conn, &fts_table, id, written)
    } else {
        upsert_sqlite(conn, &fts_table, id, written)
    }
}

/// Each column the upsert writes, paired with its text: the tsvectors (each
/// over its member columns' `texts`) on Postgres, the indexed `columns`
/// themselves on `SQLite`.
fn written_columns(
    conn: &dyn DbConnection,
    index: &FtsIndex<'_>,
    columns: Vec<FtsColumn>,
    texts: Vec<String>,
) -> Result<Vec<(String, String)>> {
    if !conn.is_postgres() {
        return Ok(columns.into_iter().map(|c| c.name).zip(texts).collect());
    }

    let vectors = pg_vectors(&columns, index.locale_config)?;
    let vector_texts: Vec<String> = pg_vector_texts(&vectors, &texts).collect();

    Ok(vectors
        .into_iter()
        .map(|v| v.name)
        .zip(vector_texts)
        .collect())
}

/// Select the indexed columns of one row through their read expressions, in
/// `columns` order, NULLs as `''`.
fn read_indexed_row(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    columns: &[FtsColumn],
) -> Result<Option<DbRow>> {
    let select: Vec<String> = columns
        .iter()
        .map(|c| format!("COALESCE({}, '')", c.read_expr))
        .collect();
    let sql = format!(
        "SELECT {} FROM \"{slug}\" WHERE id = {}",
        select.join(", "),
        conn.placeholder(1)
    );

    conn.query_one(&sql, &[DbValue::Text(id.to_string())])
        .with_context(|| format!("FTS row read from {slug}"))
}

/// Upsert into Postgres FTS: each `(tsvector column, text)` of `vectors`
/// built with `to_tsvector`, ON CONFLICT replacing every one.
fn upsert_postgres(
    conn: &dyn DbConnection,
    fts_table: &str,
    id: &str,
    vectors: Vec<(String, String)>,
) -> Result<()> {
    let names: Vec<String> = vectors.iter().map(|(name, _)| quote_ident(name)).collect();
    let values: Vec<String> = (0..vectors.len())
        .map(|i| pg_tsvector(&conn.placeholder(i + 2)))
        .collect();
    let updates: Vec<String> = names
        .iter()
        .map(|name| format!("{name} = EXCLUDED.{name}"))
        .collect();

    let sql = format!(
        "INSERT INTO {fts_table}(id, {}) VALUES ({}, {}) ON CONFLICT (id) DO UPDATE SET {}",
        names.join(", "),
        conn.placeholder(1),
        values.join(", "),
        updates.join(", ")
    );

    let mut params = vec![DbValue::Text(id.to_string())];
    params.extend(vectors.into_iter().map(|(_, text)| DbValue::Text(text)));

    conn.execute(&sql, &params)
        .with_context(|| format!("FTS upsert in {fts_table}"))?;

    Ok(())
}

/// Upsert into `SQLite` FTS5 (delete + insert, no ON CONFLICT support):
/// `columns` pairs each indexed column with its text.
fn upsert_sqlite(
    conn: &dyn DbConnection,
    fts_table: &str,
    id: &str,
    columns: Vec<(String, String)>,
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

    let quoted_cols = columns
        .iter()
        .map(|(column, _)| quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ");

    let mut values: Vec<DbValue> = vec![DbValue::Text(id.to_string())];
    values.extend(columns.into_iter().map(|(_, text)| DbValue::Text(text)));

    let placeholders = placeholder_list(conn, values.len());
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
        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
        )
        .unwrap();

        insert_post(&conn, "new1", "Unique Title", "Some content");
        fts_upsert(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
            "new1",
        )
        .unwrap();

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
        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
        )
        .unwrap();

        conn.execute(
            "UPDATE posts SET title = ?1 WHERE id = ?2",
            &[DbValue::Text("New Title".into()), DbValue::Text("1".into())],
        )
        .unwrap();
        fts_upsert(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
            "1",
        )
        .unwrap();

        let old_results = fts_match_ids(&conn, "posts", "Old", 10).unwrap();
        assert!(old_results.is_empty());

        let new_results = fts_match_ids(&conn, "posts", "New", 10).unwrap();
        assert_eq!(new_results, vec!["1"]);
    }

    #[test]
    fn upsert_noop_no_fts_table() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);
        fts_upsert(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
            "1",
        )
        .unwrap();
    }

    /// A vanished row drops its index entry instead of indexing empty text.
    #[test]
    fn upsert_of_a_missing_row_removes_the_index_entry() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "gone", "Ephemeral", "");
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
        )
        .unwrap();
        assert_eq!(
            fts_match_ids(&conn, "posts", "Ephemeral", 10).unwrap(),
            vec!["gone"]
        );

        conn.execute("DELETE FROM posts WHERE id = 'gone'", &[])
            .unwrap();
        fts_upsert(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
            "gone",
        )
        .unwrap();

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
        sync_fts_table(&conn, &FtsIndex::builder("posts", &def, &locale).build()).unwrap();

        conn.execute(
            "INSERT INTO posts (id, title__en, title__de) VALUES ('doc1', 'English Title', 'Deutscher Titel')",
            &[],
        )
        .unwrap();
        fts_upsert(
            &conn,
            &FtsIndex::builder("posts", &def, &locale).build(),
            "doc1",
        )
        .unwrap();

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
        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
        )
        .unwrap();

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

        fts_upsert(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
            "1",
        )
        .unwrap();

        let results = fts_match_ids(&conn, "posts", "Searchable", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        let results = fts_match_ids(&conn, "posts", "paragraph", 10).unwrap();
        assert!(results.is_empty());
    }

    /// Regression: an HTML rich text column was indexed as-is, so markup
    /// (tag names, class names, link targets) matched searches.
    #[test]
    fn fts_upsert_html_richtext_indexes_text_only() {
        let (_dir, conn) = setup_db();
        conn.execute_batch("ALTER TABLE posts ADD COLUMN content TEXT")
            .unwrap();

        let mut def = simple_def(vec![
            text_field("title"),
            FieldDefinition::builder("content", FieldType::Richtext).build(),
        ]);
        def.admin.list_searchable_fields = vec!["title".into(), "content".into()];
        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO posts (id, title, content, created_at, updated_at) VALUES (?1, ?2, ?3, datetime('now'), datetime('now'))",
            &[
                DbValue::Text("h1".into()),
                DbValue::Text("Test".into()),
                DbValue::Text(r#"<p class="lead">Visible <a href="/hidden">words</a></p>"#.into()),
            ],
        )
        .unwrap();

        fts_upsert(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
            "h1",
        )
        .unwrap();

        assert_eq!(
            fts_match_ids(&conn, "posts", "Visible", 10).unwrap(),
            vec!["h1"]
        );
        assert_eq!(
            fts_match_ids(&conn, "posts", "words", 10).unwrap(),
            vec!["h1"]
        );
        assert!(
            fts_match_ids(&conn, "posts", "lead", 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            fts_match_ids(&conn, "posts", "hidden", 10)
                .unwrap()
                .is_empty()
        );
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

        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
        )
        .unwrap();

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

        fts_upsert(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale())
                .registry(Some(&registry))
                .build(),
            "rg1",
        )
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
        let result = fts_upsert(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale())
                .registry(Some(&registry))
                .build(),
            "1",
        );
        assert!(result.is_ok(), "should be a no-op when no FTS table exists");
    }

    #[test]
    fn fts_upsert_with_registry_nil_inputs_use_plain_extraction() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
        )
        .unwrap();

        insert_post(&conn, "plain1", "Plain text", "");
        fts_upsert(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
            "plain1",
        )
        .unwrap();

        let results = fts_match_ids(&conn, "posts", "Plain", 10).unwrap();
        assert_eq!(results, vec!["plain1"]);
    }

    /// A non-text column value (NULL here) indexes as empty, never as an error.
    #[test]
    fn fts_upsert_null_column_indexes_as_empty() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO posts (id, title, body, created_at, updated_at) VALUES ('n1', 'Only', NULL, '', '')",
            &[],
        )
        .unwrap();
        fts_upsert(
            &conn,
            &FtsIndex::builder("posts", &def, &no_locale()).build(),
            "n1",
        )
        .unwrap();

        assert_eq!(
            fts_match_ids(&conn, "posts", "Only", 10).unwrap(),
            vec!["n1"]
        );
    }
}
