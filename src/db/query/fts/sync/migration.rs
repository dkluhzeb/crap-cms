//! Migration-time FTS table sync: drop + recreate the FTS5 / tsvector table
//! and bulk-populate from the main table.

use std::collections::HashSet;

use anyhow::{Context as _, Result, bail};

use crate::config::LocaleConfig;
use crate::core::CollectionDefinition;
use crate::db::query::fts::fields::{get_fts_columns, json_richtext_columns};
use crate::db::query::fts::prosemirror::extract_prosemirror_text;
use crate::db::query::fts::search::fts_table_name;
use crate::db::query::is_valid_identifier;
use crate::db::{DbConnection, DbValue};

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
    let fts_fields = get_fts_columns(def, locale_config)?;

    // Validate field names BEFORE dropping the old table — if validation fails,
    // the existing FTS index is preserved rather than silently lost.
    for f in &fts_fields {
        if !is_valid_identifier(f) {
            bail!(
                "Invalid FTS field name '{}': must be alphanumeric/underscore",
                f
            );
        }
    }

    // Always drop existing FTS table first
    conn.execute_batch_ddl(&format!("DROP TABLE IF EXISTS {}", fts_table))
        .with_context(|| format!("Failed to drop FTS table {}", fts_table))?;

    if fts_fields.is_empty() {
        return Ok(());
    }

    let field_list = fts_fields.join(", ");

    match conn.kind() {
        "postgres" => {
            // Create regular table with a single tsvector column
            let create_sql = format!(
                "CREATE TABLE {} (id TEXT PRIMARY KEY, tsv TSVECTOR)",
                fts_table
            );
            conn.execute_batch_ddl(&create_sql)
                .with_context(|| format!("Failed to create FTS table {}", fts_table))?;

            // Create GIN index for fast tsvector lookups
            let index_sql = format!(
                "CREATE INDEX IF NOT EXISTS idx_{}_tsv ON {} USING GIN(tsv)",
                fts_table, fts_table
            );
            conn.execute_batch_ddl(&index_sql)
                .with_context(|| format!("Failed to create GIN index on {}", fts_table))?;
        }
        _ => {
            // Create FTS5 virtual table
            let create_sql = format!(
                "CREATE VIRTUAL TABLE {} USING fts5(id UNINDEXED, {})",
                fts_table, field_list
            );
            conn.execute_batch_ddl(&create_sql)
                .with_context(|| format!("Failed to create FTS table {}", fts_table))?;
        }
    }

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
    let coalesce_fields: Vec<String> = fts_fields
        .iter()
        .map(|f| format!("COALESCE({}, '')", f))
        .collect();

    let insert_sql = match conn.kind() {
        "postgres" => {
            let tsvector_expr = format!(
                "to_tsvector('simple', {})",
                coalesce_fields.join(" || ' ' || ")
            );
            format!(
                "INSERT INTO {}(id, tsv) SELECT id, {} FROM \"{}\"",
                fts_table, tsvector_expr, slug
            )
        }
        _ => format!(
            "INSERT INTO {}(id, {}) SELECT id, {} FROM \"{}\"",
            fts_table,
            field_list,
            coalesce_fields.join(", "),
            slug
        ),
    };

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
    json_rt_cols: &HashSet<String>,
) -> Result<()> {
    let select_fields: Vec<String> = fts_fields
        .iter()
        .map(|f| format!("COALESCE({}, '')", f))
        .collect();
    let select_sql = format!("SELECT id, {} FROM \"{}\"", select_fields.join(", "), slug);

    let db_rows = conn
        .query_all(&select_sql, &[])
        .with_context(|| format!("Failed to query {} for FTS population", slug))?;

    let is_postgres = conn.kind() == "postgres";

    let insert_sql = if is_postgres {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        format!(
            "INSERT INTO {}(id, tsv) VALUES ({}, to_tsvector('simple', {}))",
            fts_table, p1, p2
        )
    } else {
        let placeholders: Vec<String> = (1..=fts_fields.len() + 1)
            .map(|i| conn.placeholder(i))
            .collect();
        format!(
            "INSERT INTO {}(id, {}) VALUES ({})",
            fts_table,
            field_list,
            placeholders.join(", ")
        )
    };

    for row in db_rows {
        let Some(id) = row.opt_text_at(0) else {
            continue;
        };

        let mut field_texts: Vec<String> = Vec::with_capacity(fts_fields.len());

        for (i, col_name) in fts_fields.iter().enumerate() {
            let raw = row.text_at(i + 1).unwrap_or("").to_string();

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
            field_texts.push(text);
        }

        if is_postgres {
            // Concatenate all field texts into a single string for the tsvector
            let combined = field_texts.join(" ");
            let params = vec![DbValue::Text(id), DbValue::Text(combined)];
            conn.execute(&insert_sql, &params)
                .with_context(|| format!("FTS bulk insert in {}", fts_table))?;
        } else {
            let mut params: Vec<DbValue> = vec![DbValue::Text(id)];
            for text in field_texts {
                params.push(DbValue::Text(text));
            }
            conn.execute(&insert_sql, &params)
                .with_context(|| format!("FTS bulk insert in {}", fts_table))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::config::LocaleConfig;
    use crate::core::field::*;
    use crate::db::DbConnection;
    use crate::db::DbValue;
    use crate::db::query::fts::search::fts_search;
    use crate::db::query::fts::sync::helpers::get_fts_table_columns;
    use crate::db::query::fts::sync::test_helpers::*;
    use crate::db::{BoxedConnection, pool};
    use tempfile::TempDir;

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
            .and_then(|row| row.i64_at(0).map(|n| n != 0))
            .unwrap_or(false);
        assert!(exists);

        let count = conn
            .query_one("SELECT COUNT(*) FROM _fts_posts", &[])
            .unwrap()
            .and_then(|row| row.i64_at(0))
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
            .and_then(|row| row.i64_at(0).map(|n| n != 0))
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
        let conn: BoxedConnection = p.get().unwrap();
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
        let conn: BoxedConnection = p.get().unwrap();
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
}
