//! Update operation and its helper.

use anyhow::{Context as _, Result, anyhow};
use std::collections::HashMap;

use crate::core::{CollectionDefinition, Document, FieldDefinition, FieldType};
use crate::db::{
    DbConnection, DbValue, LocaleContext,
    query::{coerce_value, locale_write_column, read::find_by_id_raw},
};

/// Update a document by ID. Returns the updated document.
pub fn update(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string();

    let mut col = UpdateCollector::new();

    collect_update_params(&def.fields, data, &locale_ctx, &mut col, conn, "");

    if def.timestamps {
        col.set_clauses
            .push(format!("updated_at = {}", conn.placeholder(col.idx)));
        col.params.push(DbValue::Text(now));
        col.idx += 1;
    }

    if col.set_clauses.is_empty() {
        return find_by_id_raw(conn, slug, def, id, locale_ctx)?
            .ok_or_else(|| anyhow!("Document not found"));
    }

    let sql = format!(
        "UPDATE {} SET {} WHERE id = {}",
        slug,
        col.set_clauses.join(", "),
        conn.placeholder(col.idx)
    );
    col.params.push(DbValue::Text(id.to_string()));

    conn.execute(&sql, &col.params)
        .with_context(|| format!("Failed to update document {} in '{}'", id, slug))?;

    find_by_id_raw(conn, slug, def, id, locale_ctx)?
        .ok_or_else(|| anyhow!("Document not found after update"))
}

/// Partial update: like [`update`] but skips absent checkbox fields instead of
/// defaulting them to 0. Used for bulk updates where not all fields are provided.
pub fn update_partial(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string();

    let mut col = UpdateCollector::new_partial();

    collect_update_params(&def.fields, data, &locale_ctx, &mut col, conn, "");

    if def.timestamps {
        col.set_clauses
            .push(format!("updated_at = {}", conn.placeholder(col.idx)));
        col.params.push(DbValue::Text(now));
        col.idx += 1;
    }

    if col.set_clauses.is_empty() {
        return find_by_id_raw(conn, slug, def, id, locale_ctx)?
            .ok_or_else(|| anyhow!("Document not found"));
    }

    let sql = format!(
        "UPDATE {} SET {} WHERE id = {}",
        slug,
        col.set_clauses.join(", "),
        conn.placeholder(col.idx)
    );
    col.params.push(DbValue::Text(id.to_string()));

    conn.execute(&sql, &col.params)
        .with_context(|| format!("Failed to update document {} in '{}'", id, slug))?;

    find_by_id_raw(conn, slug, def, id, locale_ctx)?
        .ok_or_else(|| anyhow!("Document not found after update"))
}

/// Accumulates SET clauses and parameter values for an UPDATE statement.
pub(in crate::db::query) struct UpdateCollector {
    pub set_clauses: Vec<String>,
    pub params: Vec<DbValue>,
    pub idx: usize,
    /// When true, absent checkbox fields are skipped instead of defaulting to 0.
    /// Used in bulk updates where not all fields are provided.
    pub skip_absent_checkboxes: bool,
}

impl UpdateCollector {
    pub fn new() -> Self {
        Self {
            set_clauses: Vec::new(),
            params: Vec::new(),
            idx: 1,
            skip_absent_checkboxes: false,
        }
    }

    /// Create a collector that skips absent checkboxes (for bulk/partial updates).
    pub fn new_partial() -> Self {
        Self {
            set_clauses: Vec::new(),
            params: Vec::new(),
            idx: 1,
            skip_absent_checkboxes: true,
        }
    }
}

/// Recursively collect SET clauses + params for UPDATE.
/// Handles arbitrary nesting: Group (prefixed), Row/Collapsible/Tabs (promoted flat).
pub(in crate::db::query) fn collect_update_params(
    fields: &[FieldDefinition],
    data: &HashMap<String, String>,
    locale_ctx: &Option<&LocaleContext>,
    collector: &mut UpdateCollector,
    conn: &dyn DbConnection,
    prefix: &str,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                collect_update_params(
                    &field.fields,
                    data,
                    locale_ctx,
                    collector,
                    conn,
                    &new_prefix,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_update_params(&field.fields, data, locale_ctx, collector, conn, prefix);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_update_params(&tab.fields, data, locale_ctx, collector, conn, prefix);
                }
            }
            _ => {
                if !field.has_parent_column() {
                    continue;
                }
                let data_key = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                let col_name = locale_write_column(&data_key, field, locale_ctx);

                if let Some(value) = data.get(&data_key) {
                    collector.set_clauses.push(format!(
                        "{} = {}",
                        col_name,
                        conn.placeholder(collector.idx)
                    ));
                    collector
                        .params
                        .push(coerce_value(&field.field_type, value));
                    collector.idx += 1;
                } else if field.field_type == FieldType::Checkbox
                    && !collector.skip_absent_checkboxes
                {
                    collector.set_clauses.push(format!(
                        "{} = {}",
                        col_name,
                        conn.placeholder(collector.idx)
                    ));
                    collector.params.push(DbValue::Integer(0));
                    collector.idx += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::create::create;
    use super::*;
    use crate::config::CrapConfig;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::{BoxedConnection, pool};
    use tempfile::TempDir;

    fn setup_db(ddl: &str) -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(ddl).unwrap();
        (dir, conn)
    }

    fn posts_ddl() -> &'static str {
        "CREATE TABLE posts (
            id TEXT PRIMARY KEY,
            title TEXT,
            status TEXT,
            created_at TEXT,
            updated_at TEXT
        )"
    }

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Text).build(),
        ];
        def
    }

    #[test]
    fn update_basic() {
        let (_dir, conn) = setup_db(posts_ddl());
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Original".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let id = doc.id.clone();

        let mut update_data = HashMap::new();
        update_data.insert("title".to_string(), "Updated".to_string());

        let updated = update(&conn, "posts", &def, &id, &update_data, None).unwrap();
        assert_eq!(updated.get_str("title"), Some("Updated"));
    }

    #[test]
    fn update_preserves_unset_fields() {
        let (_dir, conn) = setup_db(posts_ddl());
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "My Title".to_string());
        data.insert("status".to_string(), "draft".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let id = doc.id.clone();

        // Update only title, not status
        let mut update_data = HashMap::new();
        update_data.insert("title".to_string(), "New Title".to_string());

        let updated = update(&conn, "posts", &def, &id, &update_data, None).unwrap();
        assert_eq!(updated.get_str("title"), Some("New Title"));
        assert_eq!(
            updated.get_str("status"),
            Some("draft"),
            "status should be preserved"
        );
    }

    #[test]
    fn update_nonexistent_id() {
        let (_dir, conn) = setup_db(posts_ddl());
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Something".to_string());

        let result = update(&conn, "posts", &def, "nonexistent-id", &data, None);
        assert!(result.is_err(), "Updating non-existent ID should error");
    }

    #[test]
    fn update_with_group_fields() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                meta__color TEXT,
                meta__size TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );
        conn.execute(
            "INSERT INTO posts (id, meta__color, meta__size, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[
                DbValue::Text("p1".into()),
                DbValue::Text("blue".into()),
                DbValue::Text("small".into()),
                DbValue::Text("2024-01-01".into()),
                DbValue::Text("2024-01-01".into()),
            ],
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("color", FieldType::Text).build(),
                    FieldDefinition::builder("size", FieldType::Text).build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = HashMap::new();
        data.insert("meta__color".to_string(), "green".to_string());

        let doc = update(&conn, "posts", &def, "p1", &data, None).unwrap();
        assert_eq!(doc.get_str("meta__color"), Some("green"));
        // Unset sub-field should be preserved
        assert_eq!(doc.get_str("meta__size"), Some("small"));
    }

    #[test]
    fn update_without_timestamps() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                name TEXT
            )",
        );
        conn.execute(
            "INSERT INTO events (id, name) VALUES (?1, ?2)",
            &[DbValue::Text("e1".into()), DbValue::Text("Original".into())],
        )
        .unwrap();

        let mut def = CollectionDefinition::new("events");
        def.timestamps = false;
        def.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
        let def = def;

        let mut data = HashMap::new();
        data.insert("name".to_string(), "Updated".to_string());

        let doc = update(&conn, "events", &def, "e1", &data, None).unwrap();
        assert_eq!(doc.get_str("name"), Some("Updated"));
    }

    #[test]
    fn update_empty_data_returns_existing() {
        let (_dir, conn) = setup_db(posts_ddl());
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "MyTitle".to_string());
        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let id = doc.id.clone();

        // Update with no data and timestamps disabled — should return existing doc
        let mut no_ts_def = test_def();
        no_ts_def.timestamps = false;
        let empty_data = HashMap::new();
        let result = update(&conn, "posts", &no_ts_def, &id, &empty_data, None).unwrap();
        assert_eq!(result.get_str("title"), Some("MyTitle"));
    }

    #[test]
    fn update_with_tabs_containing_group() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                social__github TEXT,
                social__twitter TEXT,
                body TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );
        conn.execute(
            "INSERT INTO posts (id, social__github, social__twitter, body, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            &[
                DbValue::Text("p1".into()),
                DbValue::Text("https://github.com".into()),
                DbValue::Text("@old".into()),
                DbValue::Text("Old body".into()),
                DbValue::Text("2024-01-01".into()),
                DbValue::Text("2024-01-01".into()),
            ],
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new(
                        "Social",
                        vec![
                            FieldDefinition::builder("social", FieldType::Group)
                                .fields(vec![
                                    FieldDefinition::builder("github", FieldType::Text).build(),
                                    FieldDefinition::builder("twitter", FieldType::Text).build(),
                                ])
                                .build(),
                        ],
                    ),
                    FieldTab::new(
                        "Content",
                        vec![FieldDefinition::builder("body", FieldType::Text).build()],
                    ),
                ])
                .build(),
        ];
        let def = def;

        // Only update twitter, leave github and body untouched
        let mut data = HashMap::new();
        data.insert("social__twitter".to_string(), "@new".to_string());

        let doc = update(&conn, "posts", &def, "p1", &data, None).unwrap();
        assert_eq!(doc.get_str("social__twitter"), Some("@new"));
        assert_eq!(
            doc.get_str("social__github"),
            Some("https://github.com"),
            "github should be preserved"
        );
        assert_eq!(
            doc.get_str("body"),
            Some("Old body"),
            "body should be preserved"
        );
    }

    #[test]
    fn update_group_containing_row() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                meta__title TEXT,
                meta__slug TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );
        conn.execute(
            "INSERT INTO posts (id, meta__title, meta__slug, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[
                DbValue::Text("abc".into()),
                DbValue::Text("Old".into()),
                DbValue::Text("old".into()),
                DbValue::Text("2024-01-01".into()),
                DbValue::Text("2024-01-01".into()),
            ],
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
                        .fields(vec![
                            FieldDefinition::builder("title", FieldType::Text).build(),
                            FieldDefinition::builder("slug", FieldType::Text).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = HashMap::new();
        data.insert("meta__title".to_string(), "New Title".to_string());

        let doc = update(&conn, "posts", &def, "abc", &data, None).unwrap();
        assert_eq!(doc.get_str("meta__title"), Some("New Title"));
        assert_eq!(
            doc.get_str("meta__slug"),
            Some("old"),
            "Unset field should be preserved"
        );
    }

    // ── Regression: update_partial skips absent checkboxes ─────────────

    fn checkbox_ddl() -> &'static str {
        "CREATE TABLE items (
            id TEXT PRIMARY KEY,
            title TEXT,
            active INTEGER DEFAULT 0,
            created_at TEXT,
            updated_at TEXT
        )"
    }

    fn checkbox_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("items");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("active", FieldType::Checkbox).build(),
        ];
        def
    }

    #[test]
    fn update_resets_absent_checkbox_to_zero() {
        let (_dir, conn) = setup_db(checkbox_ddl());
        let def = checkbox_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Test".to_string());
        data.insert("active".to_string(), "1".to_string());
        let doc = create(&conn, "items", &def, &data, None).unwrap();

        // Regular update without checkbox field -> should reset to 0
        let mut update_data = HashMap::new();
        update_data.insert("title".to_string(), "Updated".to_string());
        let updated = update(&conn, "items", &def, &doc.id, &update_data, None).unwrap();
        assert_eq!(
            updated.fields.get("active").and_then(|v| v.as_i64()),
            Some(0),
            "Regular update should reset absent checkbox to 0"
        );
    }

    #[test]
    fn update_partial_preserves_absent_checkbox() {
        let (_dir, conn) = setup_db(checkbox_ddl());
        let def = checkbox_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Test".to_string());
        data.insert("active".to_string(), "1".to_string());
        let doc = create(&conn, "items", &def, &data, None).unwrap();

        // Partial update without checkbox field -> should preserve existing value
        let mut update_data = HashMap::new();
        update_data.insert("title".to_string(), "Partial".to_string());
        let updated = update_partial(&conn, "items", &def, &doc.id, &update_data, None).unwrap();
        assert_eq!(
            updated.fields.get("active").and_then(|v| v.as_i64()),
            Some(1),
            "Partial update should preserve absent checkbox value"
        );
    }

    #[test]
    fn update_partial_still_sets_provided_checkbox() {
        let (_dir, conn) = setup_db(checkbox_ddl());
        let def = checkbox_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Test".to_string());
        data.insert("active".to_string(), "1".to_string());
        let doc = create(&conn, "items", &def, &data, None).unwrap();

        // Partial update WITH checkbox field -> should update it
        let mut update_data = HashMap::new();
        update_data.insert("active".to_string(), "0".to_string());
        let updated = update_partial(&conn, "items", &def, &doc.id, &update_data, None).unwrap();
        assert_eq!(
            updated.fields.get("active").and_then(|v| v.as_i64()),
            Some(0),
            "Partial update with explicit checkbox value should set it"
        );
    }
}
