//! Migration-time FTS table sync: drop + recreate the FTS5 / tsvector table
//! and bulk-populate from the main table.

use anyhow::{Context as _, Result, bail};

use crate::db::query::fts::fields::get_fts_columns;
use crate::db::query::fts::index::{ColumnText, FtsIndex};
use crate::db::query::fts::search::fts_table_name;
use crate::db::query::fts::sync::helpers::pg_tsvector;
use crate::db::query::helpers::{placeholder_list, quote_ident};
use crate::db::query::is_valid_identifier;
use crate::db::{DbConnection, DbValue};

/// The FTS table being rebuilt and the columns it indexes.
struct IndexTable<'a> {
    name: &'a str,
    columns: &'a [String],
    /// `columns` quoted and comma-joined, for the INSERT column list.
    column_list: &'a str,
}

/// Drop and recreate `index`'s FTS table, then bulk-populate it from the main
/// table.
///
/// Called during migration (startup). Always rebuilds fresh — avoids drift detection.
/// If there are no indexable columns, drops the FTS table if it exists.
///
/// # Errors
///
/// Returns a backend error if any DROP, CREATE, or INSERT fails.
pub fn sync_fts_table(conn: &dyn DbConnection, index: &FtsIndex<'_>) -> Result<()> {
    let fts_table = fts_table_name(index.slug);
    let fts_fields = get_fts_columns(index.def, index.locale_config)?;

    // Validate field names BEFORE dropping the old table — if validation fails,
    // the existing FTS index is preserved rather than silently lost.
    for f in &fts_fields {
        if !is_valid_identifier(f) {
            bail!("Invalid FTS field name '{f}': must be alphanumeric/underscore");
        }
    }

    // Always drop existing FTS table first
    conn.execute_batch_ddl(&format!("DROP TABLE IF EXISTS {fts_table}"))
        .with_context(|| format!("Failed to drop FTS table {fts_table}"))?;

    if fts_fields.is_empty() {
        return Ok(());
    }

    let column_list = fts_fields
        .iter()
        .map(|f| quote_ident(f))
        .collect::<Vec<_>>()
        .join(", ");
    let table = IndexTable {
        name: &fts_table,
        columns: &fts_fields,
        column_list: &column_list,
    };

    create_fts_table(conn, &table)?;

    // Bulk populate from main table
    let column_text = ColumnText::new(index);

    if column_text.has_richtext() {
        bulk_populate_slow(conn, index.slug, &table, &column_text)
    } else {
        bulk_populate_fast(conn, index.slug, &table)
    }
}

/// Create the empty FTS table: a tsvector table with a GIN index on Postgres,
/// an FTS5 virtual table on `SQLite`.
fn create_fts_table(conn: &dyn DbConnection, table: &IndexTable<'_>) -> Result<()> {
    let fts_table = table.name;

    if !conn.is_postgres() {
        let create_sql = format!(
            "CREATE VIRTUAL TABLE {fts_table} USING fts5(id UNINDEXED, {})",
            table.column_list
        );

        return conn
            .execute_batch_ddl(&create_sql)
            .with_context(|| format!("Failed to create FTS table {fts_table}"));
    }

    // Regular table with a single tsvector column
    let create_sql = format!("CREATE TABLE {fts_table} (id TEXT PRIMARY KEY, tsv TSVECTOR)");
    conn.execute_batch_ddl(&create_sql)
        .with_context(|| format!("Failed to create FTS table {fts_table}"))?;

    // GIN index for fast tsvector lookups
    let index_sql =
        format!("CREATE INDEX IF NOT EXISTS idx_{fts_table}_tsv ON {fts_table} USING GIN(tsv)");
    conn.execute_batch_ddl(&index_sql)
        .with_context(|| format!("Failed to create GIN index on {fts_table}"))
}

/// Fast path: no rich text fields, pure SQL bulk insert.
fn bulk_populate_fast(conn: &dyn DbConnection, slug: &str, table: &IndexTable<'_>) -> Result<()> {
    let fts_table = table.name;
    let coalesce_fields: Vec<String> = table
        .columns
        .iter()
        .map(|f| format!("COALESCE({}, '')", quote_ident(f)))
        .collect();

    let insert_sql = if conn.is_postgres() {
        let tsvector_expr = pg_tsvector(&coalesce_fields.join(" || ' ' || "));
        format!("INSERT INTO {fts_table}(id, tsv) SELECT id, {tsvector_expr} FROM \"{slug}\"")
    } else {
        format!(
            "INSERT INTO {}(id, {}) SELECT id, {} FROM \"{}\"",
            fts_table,
            table.column_list,
            coalesce_fields.join(", "),
            slug
        )
    };

    conn.execute_batch(&insert_sql)
        .with_context(|| format!("Failed to populate FTS table {fts_table}"))?;

    Ok(())
}

/// Slow path: read rows and index each column's text as [`ColumnText`] reads
/// it (rich text as its plain text and custom nodes' searchable attrs).
fn bulk_populate_slow(
    conn: &dyn DbConnection,
    slug: &str,
    table: &IndexTable<'_>,
    column_text: &ColumnText<'_>,
) -> Result<()> {
    let fts_table = table.name;
    let select_fields: Vec<String> = table
        .columns
        .iter()
        .map(|f| format!("COALESCE({}, '')", quote_ident(f)))
        .collect();
    let select_sql = format!("SELECT id, {} FROM \"{}\"", select_fields.join(", "), slug);

    let db_rows = conn
        .query_all(&select_sql, &[])
        .with_context(|| format!("Failed to query {slug} for FTS population"))?;

    let insert_sql = bulk_insert_sql(conn, table);

    for row in db_rows {
        let Some(id) = row.opt_text_at(0) else {
            continue;
        };

        let field_texts = table
            .columns
            .iter()
            .enumerate()
            .map(|(i, col)| column_text.text(col, row.text_at(i + 1).unwrap_or("")));

        let mut params = vec![DbValue::Text(id)];

        if conn.is_postgres() {
            // All field texts in one string for the tsvector
            params.push(DbValue::Text(field_texts.collect::<Vec<_>>().join(" ")));
        } else {
            params.extend(field_texts.map(DbValue::Text));
        }

        conn.execute(&insert_sql, &params)
            .with_context(|| format!("FTS bulk insert in {fts_table}"))?;
    }

    Ok(())
}

/// The per-row INSERT of the slow path: `(id, tsv)` on Postgres, `id` plus
/// one placeholder per indexed column on `SQLite`.
fn bulk_insert_sql(conn: &dyn DbConnection, table: &IndexTable<'_>) -> String {
    let fts_table = table.name;

    if conn.is_postgres() {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        return format!(
            "INSERT INTO {fts_table}(id, tsv) VALUES ({p1}, {})",
            pg_tsvector(&p2)
        );
    }

    let placeholders = placeholder_list(conn, table.columns.len() + 1);
    format!(
        "INSERT INTO {fts_table}(id, {}) VALUES ({placeholders})",
        table.column_list
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::config::LocaleConfig;
    use crate::core::field::*;
    use crate::db::DbConnection;
    use crate::db::DbValue;
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
        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &LocaleConfig::default()).build(),
        )
        .unwrap();

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
        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &LocaleConfig::default()).build(),
        )
        .unwrap();

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
        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def1, &LocaleConfig::default()).build(),
        )
        .unwrap();

        let results = fts_match_ids(&conn, "posts", "Hello", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        let mut def2 = simple_def(vec![text_field("title"), text_field("body")]);
        def2.admin.list_searchable_fields = vec!["title".into(), "body".into()];
        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def2, &LocaleConfig::default()).build(),
        )
        .unwrap();

        let results = fts_match_ids(&conn, "posts", "World", 10).unwrap();
        assert_eq!(results, vec!["1"]);
    }

    #[test]
    fn sync_fts_table_rejects_invalid_field_names() {
        let (_dir, conn) = setup_db();
        // Include the bad-named fields in the definition so they aren't filtered
        // out as non-existent — tests that the SQL validation layer catches them.
        let mut def = simple_def(vec![text_field("title"), text_field("has space")]);
        def.admin.list_searchable_fields = vec!["title".into(), "has space".into()];
        let result = sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &LocaleConfig::default()).build(),
        );
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid FTS field name"),
            "Error should mention invalid field: {err_msg}"
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
        let result = sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &LocaleConfig::default()).build(),
        );
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
        sync_fts_table(&conn, &FtsIndex::builder("posts", &def, &locale).build()).unwrap();

        let cols = get_fts_table_columns(&conn, "_fts_posts").unwrap();
        assert!(cols.contains(&"title__en".to_string()));
        assert!(cols.contains(&"title__de".to_string()));
        assert!(cols.contains(&"body".to_string()));
        assert_eq!(cols.len(), 3);

        let results = fts_match_ids(&conn, "posts", "Hello", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        let results = fts_match_ids(&conn, "posts", "Hallo", 10).unwrap();
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

        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &LocaleConfig::default()).build(),
        )
        .unwrap();

        let results = fts_match_ids(&conn, "posts", "Extracted", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        let results = fts_match_ids(&conn, "posts", "paragraph", 10).unwrap();
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

        sync_fts_table(
            &conn,
            &FtsIndex::builder("posts", &def, &locale_config).build(),
        )
        .unwrap();

        let results = fts_match_ids(&conn, "posts", "locale", 10).unwrap();
        assert_eq!(results, vec!["1"]);
    }
}
