//! FTS5 search: sanitize queries, build WHERE clauses, run searches.

use anyhow::{Context as _, Result};

/// FTS5 table name for a collection.
pub(super) fn fts_table_name(slug: &str) -> String {
    format!("_fts_{}", slug)
}

/// Check if a table exists in the database.
pub(super) fn table_exists(conn: &rusqlite::Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |row| row.get(0),
    )
    .unwrap_or(false)
}

/// Sanitize a user search query for FTS5 with prefix matching.
///
/// Each whitespace-separated token is wrapped in double quotes (to escape special
/// chars like `'`, `:`, etc.) and suffixed with `*` for prefix matching. This means
/// typing "Con" matches "Conference", "Concept", etc. Tokens are joined with spaces
/// (implicit AND in FTS5).
///
/// Empty/whitespace-only input returns an empty string.
pub fn sanitize_fts_query(input: &str) -> String {
    let tokens: Vec<String> = input
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| {
            // Escape any double quotes inside the token
            let escaped = t.replace('"', "\"\"");
            // Quoted prefix query: "token" * (FTS5 prefix syntax)
            format!("\"{}\" *", escaped)
        })
        .collect();
    tokens.join(" ")
}

/// Search the FTS5 index and return matching document IDs, ordered by relevance.
///
/// Returns empty vec if the FTS table doesn't exist (graceful degradation).
/// Returns empty vec if the query is empty after sanitization.
pub fn fts_search(
    conn: &rusqlite::Connection,
    slug: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<String>> {
    let sanitized = sanitize_fts_query(query);

    if sanitized.is_empty() {
        return Ok(Vec::new());
    }

    let fts_table = fts_table_name(slug);

    // Check if FTS table exists (graceful degradation)
    if !table_exists(conn, &fts_table) {
        return Ok(Vec::new());
    }

    let sql = format!(
        "SELECT id FROM {} WHERE {} MATCH ?1 ORDER BY rank LIMIT ?2",
        fts_table, fts_table
    );

    let mut stmt = conn
        .prepare(&sql)
        .with_context(|| format!("Failed to prepare FTS search on {}", fts_table))?;

    let ids: Vec<String> = stmt
        .query_map(rusqlite::params![sanitized, limit], |row| row.get(0))
        .with_context(|| format!("FTS search on {}", fts_table))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(ids)
}

/// Build an `AND id IN (SELECT id FROM _fts_{slug} WHERE _fts_{slug} MATCH ?)` clause.
///
/// Returns `None` if the FTS table doesn't exist or search is empty.
/// Returns `Some((clause_fragment, sanitized_query))` to be appended to a WHERE.
pub fn fts_where_clause(
    conn: &rusqlite::Connection,
    slug: &str,
    search: &str,
) -> Option<(String, String)> {
    let sanitized = sanitize_fts_query(search);

    if sanitized.is_empty() {
        return None;
    }

    let fts_table = fts_table_name(slug);

    // Check if FTS table exists
    if !table_exists(conn, &fts_table) {
        return None;
    }

    let clause = format!(
        "id IN (SELECT id FROM {} WHERE {} MATCH ?)",
        fts_table, fts_table
    );
    Some((clause, sanitized))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::query::fts::sync::sync_fts_table;

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn simple_def(fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = fields;
        def
    }

    fn setup_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
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
        conn
    }

    fn insert_post(conn: &rusqlite::Connection, id: &str, title: &str, body: &str) {
        conn.execute(
            "INSERT INTO posts (id, title, body, created_at, updated_at) VALUES (?1, ?2, ?3, datetime('now'), datetime('now'))",
            rusqlite::params![id, title, body],
        ).unwrap();
    }

    // ── sanitize_fts_query ──────────────────────────────────────────────

    #[test]
    fn sanitize_basic() {
        assert_eq!(sanitize_fts_query("hello world"), "\"hello\" * \"world\" *");
    }

    #[test]
    fn sanitize_special_chars() {
        assert_eq!(sanitize_fts_query("foo's bar"), "\"foo's\" * \"bar\" *");
    }

    #[test]
    fn sanitize_empty() {
        assert_eq!(sanitize_fts_query(""), "");
        assert_eq!(sanitize_fts_query("   "), "");
    }

    #[test]
    fn sanitize_quotes() {
        assert_eq!(
            sanitize_fts_query("say \"hello\" please"),
            "\"say\" * \"\"\"hello\"\"\" * \"please\" *"
        );
    }

    #[test]
    fn sanitize_single_token() {
        assert_eq!(sanitize_fts_query("hello"), "\"hello\" *");
    }

    // ── fts_search ──────────────────────────────────────────────────────

    #[test]
    fn search_no_fts_table_returns_empty() {
        let conn = setup_db();
        let results = fts_search(&conn, "posts", "hello", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_empty_query_returns_empty() {
        let conn = setup_db();
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let results = fts_search(&conn, "posts", "", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_respects_limit() {
        let conn = setup_db();
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
        let conn = setup_db();
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
        let conn = setup_db();
        insert_post(&conn, "1", "Hello", "");
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let result = fts_where_clause(&conn, "posts", "Hello");
        assert!(result.is_some());
        let (clause, query) = result.unwrap();
        assert!(clause.contains("_fts_posts"));
        assert_eq!(query, "\"Hello\" *");
    }

    #[test]
    fn where_clause_no_fts_table() {
        let conn = setup_db();
        assert!(fts_where_clause(&conn, "posts", "Hello").is_none());
    }

    #[test]
    fn where_clause_empty_query() {
        let conn = setup_db();
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();
        assert!(fts_where_clause(&conn, "posts", "").is_none());
    }

    #[test]
    fn fts_where_clause_integrates_with_query() {
        let conn = setup_db();
        insert_post(&conn, "1", "Rust Programming", "Learn Rust");
        insert_post(&conn, "2", "Python Programming", "Learn Python");
        insert_post(&conn, "3", "Rust Web", "Web development with Rust");

        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let (clause, query) = fts_where_clause(&conn, "posts", "Rust").unwrap();

        let sql = format!(
            "SELECT id FROM posts WHERE status IS NULL AND {} ORDER BY id",
            clause
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let ids: Vec<String> = stmt
            .query_map([&query], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(ids, vec!["1", "3"]);
    }
}
