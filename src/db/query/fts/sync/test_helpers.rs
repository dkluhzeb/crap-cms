//! Shared test fixtures for sync/ submodules.

use tempfile::TempDir;

use crate::config::{CrapConfig, LocaleConfig};
use crate::core::CollectionDefinition;
use crate::core::{FieldDefinition, FieldType};
use crate::db::{BoxedConnection, DbConnection, DbValue, pool};

pub(super) fn text_field(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Text).build()
}

pub(super) fn localized_text_field(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Text)
        .localized(true)
        .build()
}

pub(super) fn simple_def(fields: Vec<FieldDefinition>) -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.fields = fields;
    def
}

pub(super) fn locale_config_en_de() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: false,
    }
}

pub(super) fn setup_db() -> (TempDir, BoxedConnection) {
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

pub(super) fn insert_post(conn: &dyn DbConnection, id: &str, title: &str, body: &str) {
    conn.execute(
        "INSERT INTO posts (id, title, body, created_at, updated_at) VALUES (?1, ?2, ?3, datetime('now'), datetime('now'))",
        &[
            DbValue::Text(id.to_string()),
            DbValue::Text(title.to_string()),
            DbValue::Text(body.to_string()),
        ],
    ).unwrap();
}

/// Test-only FTS index-membership probe: sanitized MATCH over the slug's FTS
/// table, returning matching ids. Replaces the removed ranked `fts_search`
/// for "is doc X in the index" assertions — no relevance ordering implied.
pub(crate) fn fts_match_ids(
    conn: &dyn crate::db::DbConnection,
    slug: &str,
    query: &str,
    limit: i64,
) -> anyhow::Result<Vec<String>> {
    let sanitized = crate::db::query::fts::search::sanitize_fts_query(conn, query);
    if sanitized.is_empty() {
        return Ok(Vec::new());
    }

    let table = format!("_fts_{slug}");
    let rows = conn.query_all(
        &format!("SELECT id FROM {table} WHERE {table} MATCH ?1 LIMIT ?2"),
        &[
            crate::db::DbValue::Text(sanitized),
            crate::db::DbValue::Integer(limit),
        ],
    )?;

    rows.iter().map(|r| r.get_string("id")).collect()
}
