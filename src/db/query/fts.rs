//! FTS5 full-text search helpers: index management, search, sync on writes.

use anyhow::{Context, Result};

use crate::config::LocaleConfig;
use crate::core::CollectionDefinition;
use crate::core::field::FieldType;
use crate::core::Document;

/// Determine which logical fields should be indexed in the FTS5 table.
///
/// Uses `list_searchable_fields` if configured, otherwise falls back to all
/// text-like fields (Text, Textarea, Richtext, Email, Code) at the parent level
/// (no group sub-fields, no array/block sub-fields).
pub fn get_fts_fields(def: &CollectionDefinition) -> Vec<String> {
    if !def.admin.list_searchable_fields.is_empty() {
        return def.admin.list_searchable_fields.clone();
    }

    def.fields
        .iter()
        .filter(|f| {
            matches!(
                f.field_type,
                FieldType::Text | FieldType::Textarea | FieldType::Richtext | FieldType::Email | FieldType::Code
            )
        })
        .map(|f| f.name.clone())
        .collect()
}

/// Expand logical field names to actual database column names.
///
/// For non-localized fields, the column name is the field name.
/// For localized fields, each field expands to `field__locale` for each locale.
pub fn get_fts_columns(def: &CollectionDefinition, locale_config: &LocaleConfig) -> Vec<String> {
    let logical_fields = get_fts_fields(def);
    if logical_fields.is_empty() {
        return Vec::new();
    }

    if !locale_config.is_enabled() {
        return logical_fields;
    }

    let mut columns = Vec::new();
    for field_name in &logical_fields {
        // Check if this field is localized
        let is_localized = def.fields.iter().any(|f| {
            f.name == *field_name && f.localized
        });

        if is_localized {
            for locale in &locale_config.locales {
                columns.push(format!("{}__{}", field_name, locale));
            }
        } else {
            columns.push(field_name.clone());
        }
    }
    columns
}

/// FTS5 table name for a collection.
fn fts_table_name(slug: &str) -> String {
    format!("_fts_{}", slug)
}

/// Check if a table exists in the database.
fn table_exists(conn: &rusqlite::Connection, name: &str) -> bool {
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

/// Drop and recreate the FTS5 virtual table, then bulk-populate from the main table.
///
/// Called during migration (startup). Always rebuilds fresh — avoids drift detection.
/// If there are no indexable columns, drops the FTS table if it exists.
pub fn sync_fts_table(
    conn: &rusqlite::Connection,
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
        if !super::is_valid_identifier(f) {
            anyhow::bail!("Invalid FTS field name '{}': must be alphanumeric/underscore", f);
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
        // Fast path: no JSON richtext fields, pure SQL bulk insert
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
    } else {
        // Slow path: read rows and extract plain text from JSON richtext fields
        let select_fields: Vec<String> = fts_fields
            .iter()
            .map(|f| format!("COALESCE({}, '')", f))
            .collect();
        let select_sql = format!(
            "SELECT id, {} FROM {}",
            select_fields.join(", "),
            slug
        );
        let mut stmt = conn.prepare(&select_sql)
            .with_context(|| format!("Failed to prepare FTS population query for {}", slug))?;
        let mut rows = stmt.query([])
            .with_context(|| format!("Failed to query {} for FTS population", slug))?;

        let placeholders: Vec<String> = (1..=fts_fields.len() + 1).map(|i| format!("?{}", i)).collect();
        let insert_sql = format!(
            "INSERT INTO {}(id, {}) VALUES ({})",
            fts_table, field_list, placeholders.join(", ")
        );

        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            params.push(Box::new(id));

            for (i, col_name) in fts_fields.iter().enumerate() {
                let raw: String = row.get(i + 1)?;
                let is_json_rt = json_rt_cols.contains(col_name)
                    || col_name.split("__").next()
                        .map(|base| json_rt_cols.contains(base))
                        .unwrap_or(false);
                let text = if is_json_rt && !raw.is_empty() {
                    extract_prosemirror_text(&raw)
                } else {
                    raw
                };
                params.push(Box::new(text));
            }

            let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            conn.execute(&insert_sql, rusqlite::params_from_iter(param_refs.iter()))
                .with_context(|| format!("FTS bulk insert in {}", fts_table))?;
        }
    }

    Ok(())
}

/// Get column names from the FTS table (excludes `id`).
///
/// Returns `None` if the FTS table doesn't exist or has no columns.
fn get_fts_table_columns(conn: &rusqlite::Connection, fts_table: &str) -> Option<Vec<String>> {
    if !table_exists(conn, fts_table) {
        return None;
    }

    // Use PRAGMA table_info (not table_xinfo) — table_xinfo includes hidden
    // virtual columns like the table name and rank which aren't real data columns.
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({})", fts_table))
        .ok()?;
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .ok()?
        .filter_map(|r| r.ok())
        .filter(|name| name != "id")
        .collect();

    if cols.is_empty() { None } else { Some(cols) }
}

/// Extract plain text from a ProseMirror JSON document.
///
/// Recursively walks the JSON tree collecting `{ "type": "text", "text": "..." }` nodes.
/// Returns concatenated plain text with spaces between nodes.
/// Returns an empty string for invalid input.
pub fn extract_prosemirror_text(json_str: &str) -> String {
    fn collect_text(value: &serde_json::Value, out: &mut Vec<String>) {
        let obj = match value.as_object() {
            Some(o) => o,
            None => return,
        };
        if obj.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                out.push(text.to_string());
            }
        }
        if let Some(content) = obj.get("content").and_then(|c| c.as_array()) {
            for child in content {
                collect_text(child, out);
            }
        }
    }

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let mut parts = Vec::new();
    collect_text(&parsed, &mut parts);
    parts.join(" ")
}

/// Extract text from ProseMirror JSON, including text from custom node attrs.
///
/// `node_searchable` maps node type names to their searchable attribute names.
/// When a node matches, its attr values are extracted as text in addition to
/// walking children.
pub fn extract_prosemirror_text_with_nodes(
    json_str: &str,
    node_searchable: &std::collections::HashMap<&str, Vec<&str>>,
) -> String {
    fn collect_text_with_nodes(
        value: &serde_json::Value,
        node_searchable: &std::collections::HashMap<&str, Vec<&str>>,
        out: &mut Vec<String>,
    ) {
        let obj = match value.as_object() {
            Some(o) => o,
            None => return,
        };
        let node_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if node_type == "text" {
            if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                out.push(text.to_string());
            }
        }
        // Check for custom node with searchable attrs
        if let Some(searchable) = node_searchable.get(node_type) {
            if let Some(attrs) = obj.get("attrs").and_then(|a| a.as_object()) {
                for attr_name in searchable {
                    if let Some(val) = attrs.get(*attr_name).and_then(|v| v.as_str()) {
                        if !val.is_empty() {
                            out.push(val.to_string());
                        }
                    }
                }
            }
        }
        if let Some(content) = obj.get("content").and_then(|c| c.as_array()) {
            for child in content {
                collect_text_with_nodes(child, node_searchable, out);
            }
        }
    }

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let mut parts = Vec::new();
    collect_text_with_nodes(&parsed, node_searchable, &mut parts);
    parts.join(" ")
}

/// Build a set of column names that are JSON-format richtext fields.
/// Checks both bare field names and locale-expanded variants (`field__locale`).
fn json_richtext_columns(def: &CollectionDefinition) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for f in &def.fields {
        if f.field_type == FieldType::Richtext
            && f.admin.richtext_format.as_deref() == Some("json")
        {
            set.insert(f.name.clone());
        }
    }
    set
}

/// Build a map of node type name → searchable attr names from collection definition
/// and registry. Used for FTS extraction of custom richtext node content.
fn build_node_searchable_map<'a>(
    def: Option<&'a CollectionDefinition>,
    registry: Option<&'a crate::core::Registry>,
) -> std::collections::HashMap<&'a str, Vec<&'a str>> {
    let mut map = std::collections::HashMap::new();
    let (def, registry) = match (def, registry) {
        (Some(d), Some(r)) => (d, r),
        _ => return map,
    };
    for field in &def.fields {
        if field.field_type == FieldType::Richtext
            && field.admin.richtext_format.as_deref() == Some("json")
        {
            for node_name in &field.admin.nodes {
                if let Some(node_def) = registry.get_richtext_node(node_name) {
                    if !node_def.searchable_attrs.is_empty() {
                        map.insert(
                            node_def.name.as_str(),
                            node_def.searchable_attrs.iter().map(|s| s.as_str()).collect(),
                        );
                    }
                }
            }
        }
    }
    map
}

/// Insert or update a document in the FTS index.
///
/// Deletes the existing row (if any) then inserts fresh data.
/// No-op if the FTS table doesn't exist. Column list is read from the FTS table
/// at runtime, so callers don't need locale awareness.
///
/// If `def` is provided, JSON-format richtext fields are extracted to plain text.
pub fn fts_upsert(
    conn: &rusqlite::Connection,
    slug: &str,
    doc: &Document,
    def: Option<&CollectionDefinition>,
) -> Result<()> {
    fts_upsert_with_registry(conn, slug, doc, def, None)
}

/// Like `fts_upsert`, but accepts an optional registry for resolving custom
/// richtext node searchable attrs.
pub fn fts_upsert_with_registry(
    conn: &rusqlite::Connection,
    slug: &str,
    doc: &Document,
    def: Option<&CollectionDefinition>,
    registry: Option<&crate::core::Registry>,
) -> Result<()> {
    let fts_table = fts_table_name(slug);

    let fts_cols = match get_fts_table_columns(conn, &fts_table) {
        Some(cols) => cols,
        None => return Ok(()),
    };

    let json_rt_cols = def.map(|d| json_richtext_columns(d))
        .unwrap_or_default();

    // Build searchable attrs map for custom richtext nodes
    let node_searchable = build_node_searchable_map(def, registry);

    // Delete existing row
    conn.execute(
        &format!("DELETE FROM {} WHERE id = ?1", fts_table),
        [&doc.id],
    )
    .with_context(|| format!("FTS delete before upsert in {}", fts_table))?;

    // Insert new row
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    values.push(Box::new(doc.id.clone()));

    for col_name in &fts_cols {
        let raw = doc
            .fields
            .get(col_name)
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Check if this column is a JSON-format richtext field
        // (either exact match or the base name before "__locale" suffix)
        let is_json_rt = json_rt_cols.contains(col_name)
            || col_name.split("__").next()
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
        values.push(Box::new(text));
    }

    let placeholders: Vec<String> = (1..=values.len()).map(|i| format!("?{}", i)).collect();
    let field_list: String = fts_cols.join(", ");
    let sql = format!(
        "INSERT INTO {}(id, {}) VALUES ({})",
        fts_table,
        field_list,
        placeholders.join(", ")
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|p| p.as_ref()).collect();
    conn.execute(&sql, rusqlite::params_from_iter(param_refs.iter()))
        .with_context(|| format!("FTS upsert in {}", fts_table))?;

    Ok(())
}

/// Delete a document from the FTS index.
///
/// No-op if the FTS table doesn't exist.
pub fn fts_delete(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
) -> Result<()> {
    let fts_table = fts_table_name(slug);

    if !table_exists(conn, &fts_table) {
        return Ok(());
    }

    conn.execute(
        &format!("DELETE FROM {} WHERE id = ?1", fts_table),
        [id],
    )
    .with_context(|| format!("FTS delete in {}", fts_table))?;

    Ok(())
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
    use std::collections::HashMap;

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        }
    }

    fn simple_def(fields: Vec<FieldDefinition>) -> CollectionDefinition {
        CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields,
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        }
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

    // ── get_fts_fields ──────────────────────────────────────────────────

    #[test]
    fn get_fts_fields_uses_searchable_fields() {
        let def = CollectionDefinition {
            admin: CollectionAdmin {
                list_searchable_fields: vec!["title".into(), "body".into()],
                ..Default::default()
            },
            ..simple_def(vec![
                text_field("title"),
                text_field("body"),
                FieldDefinition {
                    name: "count".to_string(),
                    field_type: FieldType::Number,
                    ..Default::default()
                },
            ])
        };
        assert_eq!(get_fts_fields(&def), vec!["title", "body"]);
    }

    #[test]
    fn get_fts_fields_falls_back_to_text_types() {
        let def = simple_def(vec![
            text_field("title"),
            FieldDefinition {
                name: "body".to_string(),
                field_type: FieldType::Textarea,
                ..Default::default()
            },
            FieldDefinition {
                name: "count".to_string(),
                field_type: FieldType::Number,
                ..Default::default()
            },
            FieldDefinition {
                name: "email".to_string(),
                field_type: FieldType::Email,
                ..Default::default()
            },
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Richtext,
                ..Default::default()
            },
            FieldDefinition {
                name: "snippet".to_string(),
                field_type: FieldType::Code,
                ..Default::default()
            },
        ]);
        let fields = get_fts_fields(&def);
        assert_eq!(fields, vec!["title", "body", "email", "content", "snippet"]);
    }

    #[test]
    fn get_fts_fields_empty_for_no_text() {
        let def = simple_def(vec![FieldDefinition {
            name: "count".to_string(),
            field_type: FieldType::Number,
            ..Default::default()
        }]);
        assert!(get_fts_fields(&def).is_empty());
    }

    #[test]
    fn get_fts_fields_excludes_non_parent() {
        let def = simple_def(vec![
            text_field("title"),
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                fields: vec![text_field("label")],
                ..Default::default()
            },
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![text_field("description")],
                ..Default::default()
            },
        ]);
        // Only "title" at parent level — Array and Group are not text-like
        assert_eq!(get_fts_fields(&def), vec!["title"]);
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

    // ── sync_fts_table ──────────────────────────────────────────────────

    #[test]
    fn sync_creates_and_populates() {
        let conn = setup_db();
        insert_post(&conn, "1", "Hello World", "Body text");
        insert_post(&conn, "2", "Rust FTS", "Full text search");

        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        // FTS table should exist
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='_fts_posts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists);

        // Should have 2 rows
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM _fts_posts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn sync_drops_when_no_fields() {
        let conn = setup_db();
        // Create an FTS table first
        conn.execute_batch("CREATE VIRTUAL TABLE _fts_posts USING fts5(id UNINDEXED, title)")
            .unwrap();

        // Def with no text fields
        let def = simple_def(vec![FieldDefinition {
            name: "count".to_string(),
            field_type: FieldType::Number,
            ..Default::default()
        }]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        // FTS table should be gone
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='_fts_posts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!exists);
    }

    #[test]
    fn sync_rebuilds_on_field_change() {
        let conn = setup_db();
        insert_post(&conn, "1", "Hello", "World");

        // First sync with title only
        let def1 = CollectionDefinition {
            admin: CollectionAdmin {
                list_searchable_fields: vec!["title".into()],
                ..Default::default()
            },
            ..simple_def(vec![text_field("title"), text_field("body")])
        };
        sync_fts_table(&conn, "posts", &def1, &LocaleConfig::default()).unwrap();

        // Verify search works on title
        let results = fts_search(&conn, "posts", "Hello", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        // Re-sync with title + body
        let def2 = CollectionDefinition {
            admin: CollectionAdmin {
                list_searchable_fields: vec!["title".into(), "body".into()],
                ..Default::default()
            },
            ..simple_def(vec![text_field("title"), text_field("body")])
        };
        sync_fts_table(&conn, "posts", &def2, &LocaleConfig::default()).unwrap();

        // Search on body content should now work
        let results = fts_search(&conn, "posts", "World", 10).unwrap();
        assert_eq!(results, vec!["1"]);
    }

    // ── fts_upsert / fts_delete ─────────────────────────────────────────

    #[test]
    fn upsert_and_search() {
        let conn = setup_db();
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        // Insert a new doc
        insert_post(&conn, "new1", "Unique Title", "Some content");
        let doc = Document {
            id: "new1".to_string(),
            fields: HashMap::from([
                ("title".into(), serde_json::json!("Unique Title")),
                ("body".into(), serde_json::json!("Some content")),
            ]),
            created_at: None,
            updated_at: None,
        };
        fts_upsert(&conn, "posts", &doc, None).unwrap();

        let results = fts_search(&conn, "posts", "Unique", 10).unwrap();
        assert_eq!(results, vec!["new1"]);
    }

    #[test]
    fn upsert_updates_existing() {
        let conn = setup_db();
        insert_post(&conn, "1", "Old Title", "");
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        // Update the document in FTS
        let doc = Document {
            id: "1".to_string(),
            fields: HashMap::from([
                ("title".into(), serde_json::json!("New Title")),
                ("body".into(), serde_json::json!("")),
            ]),
            created_at: None,
            updated_at: None,
        };
        fts_upsert(&conn, "posts", &doc, None).unwrap();

        // Old title should not match
        let old_results = fts_search(&conn, "posts", "Old", 10).unwrap();
        assert!(old_results.is_empty());

        // New title should match
        let new_results = fts_search(&conn, "posts", "New", 10).unwrap();
        assert_eq!(new_results, vec!["1"]);
    }

    #[test]
    fn delete_removes_from_index() {
        let conn = setup_db();
        insert_post(&conn, "1", "Searchable", "");
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        // Confirm it's found
        assert_eq!(fts_search(&conn, "posts", "Searchable", 10).unwrap().len(), 1);

        // Delete
        fts_delete(&conn, "posts", "1").unwrap();
        assert!(fts_search(&conn, "posts", "Searchable", 10).unwrap().is_empty());
    }

    #[test]
    fn upsert_noop_no_fts_table() {
        let conn = setup_db();
        let doc = Document {
            id: "1".to_string(),
            fields: HashMap::new(),
            created_at: None,
            updated_at: None,
        };
        // Should be a no-op, no error (no FTS table exists)
        fts_upsert(&conn, "posts", &doc, None).unwrap();
    }

    #[test]
    fn delete_noop_no_fts_table() {
        let conn = setup_db();
        fts_delete(&conn, "posts", "1").unwrap();
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
        insert_post(&conn, "1", "Rust programming", "Learn Rust today with Rust tutorials");
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

    // ── get_fts_columns (locale) ───────────────────────────────────────

    fn locale_config_en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: false,
        }
    }

    fn localized_text_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            localized: true,
            ..Default::default()
        }
    }

    #[test]
    fn get_fts_columns_no_locale_returns_field_names() {
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        let cols = get_fts_columns(&def, &LocaleConfig::default());
        assert_eq!(cols, vec!["title", "body"]);
    }

    #[test]
    fn get_fts_columns_with_locale_expands_localized_fields() {
        let def = simple_def(vec![
            localized_text_field("title"),
            localized_text_field("body"),
        ]);
        let cols = get_fts_columns(&def, &locale_config_en_de());
        assert_eq!(cols, vec!["title__en", "title__de", "body__en", "body__de"]);
    }

    #[test]
    fn get_fts_columns_mixed_localized_and_non_localized() {
        let def = simple_def(vec![
            localized_text_field("title"),
            text_field("slug"),
        ]);
        let cols = get_fts_columns(&def, &locale_config_en_de());
        assert_eq!(cols, vec!["title__en", "title__de", "slug"]);
    }

    #[test]
    fn get_fts_columns_locale_enabled_but_no_localized_fields() {
        let def = simple_def(vec![text_field("title"), text_field("body")]);
        let cols = get_fts_columns(&def, &locale_config_en_de());
        // None of the fields are localized, so no expansion
        assert_eq!(cols, vec!["title", "body"]);
    }

    #[test]
    fn get_fts_columns_empty_when_no_text_fields() {
        let def = simple_def(vec![FieldDefinition {
            name: "count".to_string(),
            field_type: FieldType::Number,
            ..Default::default()
        }]);
        let cols = get_fts_columns(&def, &locale_config_en_de());
        assert!(cols.is_empty());
    }

    // ── sync_fts_table: invalid field names ────────────────────────────

    #[test]
    fn sync_fts_table_rejects_invalid_field_names() {
        let conn = setup_db();
        let def = CollectionDefinition {
            admin: CollectionAdmin {
                list_searchable_fields: vec!["valid".into(), "has space".into()],
                ..Default::default()
            },
            ..simple_def(vec![text_field("title")])
        };
        let result = sync_fts_table(&conn, "posts", &def, &LocaleConfig::default());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Invalid FTS field name"), "Error should mention invalid field: {}", err_msg);
    }

    #[test]
    fn sync_fts_table_rejects_sql_injection_field_names() {
        let conn = setup_db();
        let def = CollectionDefinition {
            admin: CollectionAdmin {
                list_searchable_fields: vec!["title; DROP TABLE posts".into()],
                ..Default::default()
            },
            ..simple_def(vec![text_field("title")])
        };
        let result = sync_fts_table(&conn, "posts", &def, &LocaleConfig::default());
        assert!(result.is_err());
    }

    // ── sync_fts_table with locale ─────────────────────────────────────

    #[test]
    fn sync_fts_table_creates_locale_columns() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title__en TEXT,
                title__de TEXT,
                body TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        ).unwrap();
        conn.execute(
            "INSERT INTO posts (id, title__en, title__de, body) VALUES ('1', 'Hello', 'Hallo', 'Content')",
            [],
        ).unwrap();

        let def = simple_def(vec![
            localized_text_field("title"),
            text_field("body"),
        ]);
        let locale = locale_config_en_de();
        sync_fts_table(&conn, "posts", &def, &locale).unwrap();

        // Verify FTS table has locale-expanded columns
        let cols = get_fts_table_columns(&conn, "_fts_posts").unwrap();
        assert!(cols.contains(&"title__en".to_string()));
        assert!(cols.contains(&"title__de".to_string()));
        assert!(cols.contains(&"body".to_string()));
        assert_eq!(cols.len(), 3);

        // Search in English
        let results = fts_search(&conn, "posts", "Hello", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        // Search in German
        let results = fts_search(&conn, "posts", "Hallo", 10).unwrap();
        assert_eq!(results, vec!["1"]);
    }

    // ── fts_upsert with locale columns ─────────────────────────────────

    #[test]
    fn upsert_with_locale_columns() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title__en TEXT,
                title__de TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        ).unwrap();

        let def = simple_def(vec![localized_text_field("title")]);
        let locale = locale_config_en_de();
        sync_fts_table(&conn, "posts", &def, &locale).unwrap();

        // Upsert a doc with locale columns
        let doc = Document {
            id: "doc1".to_string(),
            fields: HashMap::from([
                ("title__en".into(), serde_json::json!("English Title")),
                ("title__de".into(), serde_json::json!("Deutscher Titel")),
            ]),
            created_at: None,
            updated_at: None,
        };
        fts_upsert(&conn, "posts", &doc, None).unwrap();

        // Both languages should be searchable
        let en_results = fts_search(&conn, "posts", "English", 10).unwrap();
        assert_eq!(en_results, vec!["doc1"]);

        let de_results = fts_search(&conn, "posts", "Deutscher", 10).unwrap();
        assert_eq!(de_results, vec!["doc1"]);
    }

    // ── search combined with where clause ──────────────────────────────

    #[test]
    fn fts_where_clause_integrates_with_query() {
        let conn = setup_db();
        insert_post(&conn, "1", "Rust Programming", "Learn Rust");
        insert_post(&conn, "2", "Python Programming", "Learn Python");
        insert_post(&conn, "3", "Rust Web", "Web development with Rust");

        let def = simple_def(vec![text_field("title"), text_field("body")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        // FTS clause should produce valid SQL that filters correctly
        let (clause, query) = fts_where_clause(&conn, "posts", "Rust").unwrap();

        // Use the clause in a real query combined with another WHERE condition
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

    // ── extract_prosemirror_text ──────────────────────────────────────────

    #[test]
    fn extract_prosemirror_text_simple() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello world"}]}]}"#;
        assert_eq!(extract_prosemirror_text(json), "Hello world");
    }

    #[test]
    fn extract_prosemirror_text_nested() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello"},{"type":"text","text":" world","marks":[{"type":"strong"}]}]},{"type":"paragraph","content":[{"type":"text","text":"Second paragraph"}]}]}"#;
        assert_eq!(extract_prosemirror_text(json), "Hello  world Second paragraph");
    }

    #[test]
    fn extract_prosemirror_text_empty() {
        let json = r#"{"type":"doc","content":[]}"#;
        assert_eq!(extract_prosemirror_text(json), "");
    }

    #[test]
    fn extract_prosemirror_text_invalid() {
        assert_eq!(extract_prosemirror_text("not json"), "");
        assert_eq!(extract_prosemirror_text(""), "");
    }

    #[test]
    fn extract_prosemirror_text_with_custom_node_attrs() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello"}]},{"type":"cta","attrs":{"text":"Click me","url":"/go"}}]}"#;
        let mut node_searchable = std::collections::HashMap::new();
        node_searchable.insert("cta", vec!["text"]);
        let result = extract_prosemirror_text_with_nodes(json, &node_searchable);
        assert!(result.contains("Hello"));
        assert!(result.contains("Click me"));
        assert!(!result.contains("/go")); // url is not searchable
    }

    #[test]
    fn extract_prosemirror_text_ignores_non_searchable_attrs() {
        let json = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"Button","url":"https://example.com","style":"primary"}}]}"#;
        let mut node_searchable = std::collections::HashMap::new();
        node_searchable.insert("cta", vec!["text"]);
        let result = extract_prosemirror_text_with_nodes(json, &node_searchable);
        assert_eq!(result, "Button");
    }

    #[test]
    fn extract_prosemirror_text_with_nodes_empty_map() {
        // With empty map, behaves like the regular extract
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello"}]}]}"#;
        let node_searchable = std::collections::HashMap::new();
        let result = extract_prosemirror_text_with_nodes(json, &node_searchable);
        assert_eq!(result, "Hello");
    }

    // ── fts_upsert with JSON richtext ────────────────────────────────────

    #[test]
    fn fts_upsert_json_richtext() {
        let conn = setup_db();
        // Add a "content" richtext column to the posts table
        conn.execute_batch("ALTER TABLE posts ADD COLUMN content TEXT").unwrap();

        let mut def = simple_def(vec![
            text_field("title"),
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Richtext,
                admin: FieldAdmin {
                    richtext_format: Some("json".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        ]);
        def.admin.list_searchable_fields = vec!["title".into(), "content".into()];
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        let pm_json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Searchable text inside JSON"}]}]}"#;
        conn.execute(
            "INSERT INTO posts (id, title, content, created_at, updated_at) VALUES ('1', 'Test', ?1, datetime('now'), datetime('now'))",
            [pm_json],
        ).unwrap();

        let doc = Document {
            id: "1".to_string(),
            fields: HashMap::from([
                ("title".into(), serde_json::json!("Test")),
                ("content".into(), serde_json::json!(pm_json)),
            ]),
            created_at: None,
            updated_at: None,
        };
        fts_upsert(&conn, "posts", &doc, Some(&def)).unwrap();

        // Should find by extracted plain text
        let results = fts_search(&conn, "posts", "Searchable", 10).unwrap();
        assert_eq!(results, vec!["1"]);

        // Should NOT find by JSON structure keywords
        let results = fts_search(&conn, "posts", "paragraph", 10).unwrap();
        assert!(results.is_empty());
    }
}
