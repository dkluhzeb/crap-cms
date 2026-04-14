//! Update operation and its helper.

use std::collections::HashMap;

use anyhow::{Context as _, Result, anyhow};

use crate::{
    core::{CollectionDefinition, Document, FieldDefinition, FieldType},
    db::{
        DbConnection, DbValue, LocaleContext,
        query::{
            coerce_value,
            helpers::{
                coerce_date_value, prefixed_name, tz_column, utc_now, validate_no_null_byte,
                walk_leaf_fields,
            },
            locale_write_column,
            read::find_by_id_raw,
        },
    },
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
    update_inner(
        conn,
        slug,
        def,
        id,
        data,
        locale_ctx,
        UpdateCollector::new(),
    )
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
    update_inner(
        conn,
        slug,
        def,
        id,
        data,
        locale_ctx,
        UpdateCollector::new_partial(),
    )
}

/// Shared implementation for full and partial updates.
fn update_inner(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
    mut col: UpdateCollector,
) -> Result<Document> {
    collect_update_params(&def.fields, data, &locale_ctx, &mut col, conn)?;

    if def.timestamps {
        col.push(conn, "updated_at", DbValue::Text(utc_now()));
    }

    if col.set_clauses.is_empty() {
        return find_by_id_raw(conn, slug, def, id, locale_ctx, false)?
            .ok_or_else(|| anyhow!("Document not found"));
    }

    let sql = format!(
        "UPDATE \"{slug}\" SET {} WHERE id = {}",
        col.set_clauses.join(", "),
        conn.placeholder(col.idx)
    );

    col.params.push(DbValue::Text(id.to_string()));

    conn.execute(&sql, &col.params)
        .with_context(|| format!("Failed to update document {id} in '{slug}'"))?;

    find_by_id_raw(conn, slug, def, id, locale_ctx, false)?
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
            skip_absent_checkboxes: true,
            ..Self::new()
        }
    }

    /// Push a SET clause, its placeholder, and value.
    pub(in crate::db::query) fn push(&mut self, conn: &dyn DbConnection, col: &str, val: DbValue) {
        self.set_clauses
            .push(format!("{col} = {}", conn.placeholder(self.idx)));
        self.params.push(val);
        self.idx += 1;
    }
}

/// Collect UPDATE params for a single leaf (scalar) field.
fn collect_leaf_update(
    field: &FieldDefinition,
    data: &HashMap<String, String>,
    locale_ctx: &Option<&LocaleContext>,
    collector: &mut UpdateCollector,
    conn: &dyn DbConnection,
    prefix: &str,
    inherited_localized: bool,
) -> Result<()> {
    let data_key = prefixed_name(prefix, &field.name);
    let col_name = locale_write_column(&data_key, field, locale_ctx, inherited_localized)?;

    let Some(value) = data.get(&data_key) else {
        if field.field_type == FieldType::Checkbox && !collector.skip_absent_checkboxes {
            collector.push(conn, &col_name, DbValue::Integer(0));
        }
        return Ok(());
    };

    let is_date_tz = field.field_type == FieldType::Date && field.timezone;
    let tz_key = if is_date_tz {
        Some(tz_column(&data_key))
    } else {
        None
    };

    validate_no_null_byte(&field.field_type, &data_key, value)?;

    let db_val = match tz_key.as_ref() {
        Some(tk) => coerce_date_value(&field.field_type, value, data.get(tk).map(|s| s.as_str())),
        None => coerce_value(&field.field_type, value),
    };

    collector.push(conn, &col_name, db_val);

    if let Some(tk) = tz_key {
        let tz_col = locale_write_column(&tk, field, locale_ctx, inherited_localized)?;
        let tz_val = data.get(&tk).map(|s| s.as_str()).unwrap_or("");
        let db_val = if tz_val.is_empty() {
            DbValue::Null
        } else {
            DbValue::Text(tz_val.to_string())
        };

        collector.push(conn, &tz_col, db_val);
    }

    Ok(())
}

/// Collect SET clauses + params for UPDATE.
/// Uses `walk_leaf_fields` to handle Group/Row/Collapsible/Tabs recursion.
pub(in crate::db::query) fn collect_update_params(
    fields: &[FieldDefinition],
    data: &HashMap<String, String>,
    locale_ctx: &Option<&LocaleContext>,
    collector: &mut UpdateCollector,
    conn: &dyn DbConnection,
) -> Result<()> {
    walk_leaf_fields(
        fields,
        "",
        false,
        &mut |field, prefix, inherited_localized| {
            if field.has_parent_column() {
                collect_leaf_update(
                    field,
                    data,
                    locale_ctx,
                    collector,
                    conn,
                    prefix,
                    inherited_localized,
                )?;
            }

            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::query::write::create;
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

    // ── Timezone companion tests ─────────────────────────────────────

    #[test]
    fn update_date_with_timezone_normalizes_and_stores_tz() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                start_date TEXT,
                start_date_tz TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );
        conn.execute(
            "INSERT INTO events (id, start_date, start_date_tz, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[
                DbValue::Text("e1".into()),
                DbValue::Text("2024-01-01T12:00:00.000Z".into()),
                DbValue::Text("UTC".into()),
                DbValue::Text("2024-01-01".into()),
                DbValue::Text("2024-01-01".into()),
            ],
        )
        .unwrap();

        let mut def = CollectionDefinition::new("events");
        def.fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert("start_date".to_string(), "2024-06-15T10:00".to_string());
        data.insert("start_date_tz".to_string(), "America/Chicago".to_string());

        let doc = update(&conn, "events", &def, "e1", &data, None).unwrap();

        // 10am CDT (summer) = 3pm UTC
        assert_eq!(doc.get_str("start_date"), Some("2024-06-15T15:00:00.000Z"));
        assert_eq!(doc.get_str("start_date_tz"), Some("America/Chicago"));
    }

    #[test]
    fn update_date_timezone_without_tz_value_falls_back() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                start_date TEXT,
                start_date_tz TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );
        conn.execute(
            "INSERT INTO events (id, start_date, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
            &[
                DbValue::Text("e1".into()),
                DbValue::Text("2024-01-01T12:00:00.000Z".into()),
                DbValue::Text("2024-01-01".into()),
                DbValue::Text("2024-01-01".into()),
            ],
        )
        .unwrap();

        let mut def = CollectionDefinition::new("events");
        def.fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert("start_date".to_string(), "2024-06-15T10:00".to_string());
        // No tz value

        let doc = update(&conn, "events", &def, "e1", &data, None).unwrap();

        // Falls back to normal (treat as UTC)
        assert_eq!(doc.get_str("start_date"), Some("2024-06-15T10:00:00.000Z"));
    }
}
