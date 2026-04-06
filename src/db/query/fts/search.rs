//! FTS search: sanitize queries, build WHERE clauses, run searches.
//!
//! Supports SQLite (FTS5 MATCH) and PostgreSQL (tsvector @@ tsquery).

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbValue};

/// FTS5 table name for a collection.
pub(super) fn fts_table_name(slug: &str) -> String {
    format!("_fts_{}", slug)
}

/// Check if a table exists in the database.
pub(super) fn table_exists(conn: &dyn DbConnection, name: &str) -> bool {
    conn.table_exists(name).unwrap_or(false)
}

/// Sanitize a user search query for FTS with prefix matching.
///
/// **SQLite (FTS5):** Each token is wrapped in double quotes and suffixed with `*`
/// for prefix matching. Tokens are joined with spaces (implicit AND in FTS5).
///
/// **PostgreSQL:** Each token is lowercased, sanitized (alphanumeric only), suffixed
/// with `:*` for prefix matching, and joined with ` & ` (AND).
///
/// Empty/whitespace-only input returns an empty string.
pub fn sanitize_fts_query(conn: &dyn DbConnection, input: &str) -> String {
    let raw_tokens: Vec<&str> = input.split_whitespace().filter(|t| !t.is_empty()).collect();

    if raw_tokens.is_empty() {
        return String::new();
    }

    match conn.kind() {
        "postgres" => {
            let tokens: Vec<String> = raw_tokens
                .into_iter()
                .map(|t| {
                    // Strip non-alphanumeric chars (except underscore) to prevent
                    // tsquery injection, then append :* for prefix matching
                    let clean: String = t
                        .chars()
                        .filter(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    format!("{}:*", clean)
                })
                .filter(|t| t != ":*")
                .collect();
            tokens.join(" & ")
        }
        _ => {
            let tokens: Vec<String> = raw_tokens
                .into_iter()
                .map(|t| {
                    let escaped = t.replace('"', "\"\"");
                    format!("\"{}\" *", escaped)
                })
                .collect();
            tokens.join(" ")
        }
    }
}

/// Search the FTS5 index and return matching document IDs, ordered by relevance.
///
/// Returns empty vec if the FTS table doesn't exist (graceful degradation).
/// Returns empty vec if the query is empty after sanitization.
pub fn fts_search(
    conn: &dyn DbConnection,
    slug: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<String>> {
    let sanitized = sanitize_fts_query(conn, query);

    if sanitized.is_empty() {
        return Ok(Vec::new());
    }

    let fts_table = fts_table_name(slug);

    // Check if FTS table exists (graceful degradation)
    if !table_exists(conn, &fts_table) {
        return Ok(Vec::new());
    }

    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));

    let sql = match conn.kind() {
        "postgres" => {
            let p1_copy = conn.placeholder(1);
            format!(
                "SELECT id FROM {} WHERE tsv @@ to_tsquery('simple', {}) \
                 ORDER BY ts_rank(tsv, to_tsquery('simple', {})) DESC LIMIT {}",
                fts_table, p1, p1_copy, p2
            )
        }
        _ => format!(
            "SELECT id FROM {} WHERE {} MATCH {} ORDER BY rank LIMIT {}",
            fts_table, fts_table, p1, p2
        ),
    };

    let rows = conn
        .query_all(&sql, &[DbValue::Text(sanitized), DbValue::Integer(limit)])
        .with_context(|| format!("FTS search on {}", fts_table))?;

    let ids = rows
        .into_iter()
        .filter_map(|row| {
            if let Some(DbValue::Text(s)) = row.get_value(0) {
                Some(s.clone())
            } else {
                None
            }
        })
        .collect();

    Ok(ids)
}

/// Build an `AND id IN (SELECT id FROM _fts_{slug} WHERE _fts_{slug} MATCH ?)` clause.
///
/// Returns `None` if the FTS table doesn't exist or search is empty.
/// Returns `Some((clause_fragment, sanitized_query))` to be appended to a WHERE.
pub fn fts_where_clause(
    conn: &dyn DbConnection,
    slug: &str,
    search: &str,
    param_index: usize,
) -> Option<(String, String)> {
    let sanitized = sanitize_fts_query(conn, search);

    if sanitized.is_empty() {
        return None;
    }

    let fts_table = fts_table_name(slug);

    // Check if FTS table exists
    if !table_exists(conn, &fts_table) {
        return None;
    }

    let placeholder = conn.placeholder(param_index);

    let clause = match conn.kind() {
        "postgres" => format!(
            "id IN (SELECT id FROM {} WHERE tsv @@ to_tsquery('simple', {}))",
            fts_table, placeholder
        ),
        _ => format!(
            "id IN (SELECT id FROM {} WHERE {} MATCH {})",
            fts_table, fts_table, placeholder
        ),
    };

    Some((clause, sanitized))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrapConfig, LocaleConfig};
    use crate::core::collection::CollectionDefinition;
    use crate::core::field::FieldDefinition;
    use crate::db::migrate::collection::test_helpers::text_field;
    use crate::db::query::fts::sync::sync_fts_table;
    use crate::db::{BoxedConnection, DbValue, pool};
    use tempfile::TempDir;

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

    // ── sanitize_fts_query ──────────────────────────────────────────────

    #[test]
    fn sanitize_basic() {
        let (_dir, conn) = setup_db();
        assert_eq!(
            sanitize_fts_query(&conn, "hello world"),
            "\"hello\" * \"world\" *"
        );
    }

    #[test]
    fn sanitize_special_chars() {
        let (_dir, conn) = setup_db();
        assert_eq!(
            sanitize_fts_query(&conn, "foo's bar"),
            "\"foo's\" * \"bar\" *"
        );
    }

    #[test]
    fn sanitize_empty() {
        let (_dir, conn) = setup_db();
        assert_eq!(sanitize_fts_query(&conn, ""), "");
        assert_eq!(sanitize_fts_query(&conn, "   "), "");
    }

    #[test]
    fn sanitize_quotes() {
        let (_dir, conn) = setup_db();
        assert_eq!(
            sanitize_fts_query(&conn, "say \"hello\" please"),
            "\"say\" * \"\"\"hello\"\"\" * \"please\" *"
        );
    }

    #[test]
    fn sanitize_single_token() {
        let (_dir, conn) = setup_db();
        assert_eq!(sanitize_fts_query(&conn, "hello"), "\"hello\" *");
    }

    // ── fts_search ──────────────────────────────────────────────────────

    #[test]
    fn search_no_fts_table_returns_empty() {
        let (_dir, conn) = setup_db();
        let results = fts_search(&conn, "posts", "hello", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_empty_query_returns_empty() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let results = fts_search(&conn, "posts", "", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_respects_limit() {
        let (_dir, conn) = setup_db();
        for i in 1..=5 {
            insert_post(&conn, &format!("id{}", i), &format!("Rust post {}", i), "");
        }
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let results = fts_search(&conn, "posts", "Rust", 3).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn search_relevance_ranking() {
        let (_dir, conn) = setup_db();
        insert_post(
            &conn,
            "1",
            "Rust programming",
            "Learn Rust today with Rust tutorials",
        );
        insert_post(&conn, "2", "Python programming", "Learn Python today");
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        // "Rust" should return doc 1 first (appears in both title and body)
        let results = fts_search(&conn, "posts", "Rust", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0], "1");
    }

    // ── fts_where_clause ────────────────────────────────────────────────

    #[test]
    fn where_clause_with_fts_table() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "1", "Hello", "");
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let result = fts_where_clause(&conn, "posts", "Hello", 1);
        assert!(result.is_some());
        let (clause, query) = result.unwrap();
        assert!(clause.contains("_fts_posts"));
        assert_eq!(query, "\"Hello\" *");
    }

    #[test]
    fn where_clause_no_fts_table() {
        let (_dir, conn) = setup_db();
        assert!(fts_where_clause(&conn, "posts", "Hello", 1).is_none());
    }

    #[test]
    fn where_clause_empty_query() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();
        assert!(fts_where_clause(&conn, "posts", "", 1).is_none());
    }

    #[test]
    fn fts_where_clause_integrates_with_query() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "1", "Rust Programming", "Learn Rust");
        insert_post(&conn, "2", "Python Programming", "Learn Python");
        insert_post(&conn, "3", "Rust Web", "Web development with Rust");

        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let (clause, query) = fts_where_clause(&conn, "posts", "Rust", 1).unwrap();

        let sql = format!(
            "SELECT id FROM posts WHERE status IS NULL AND {} ORDER BY id",
            clause
        );
        let rows = conn.query_all(&sql, &[DbValue::Text(query)]).unwrap();
        let ids: Vec<String> = rows
            .into_iter()
            .filter_map(|row| {
                if let Some(DbValue::Text(s)) = row.get_value(0) {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(ids, vec!["1", "3"]);
    }
}
