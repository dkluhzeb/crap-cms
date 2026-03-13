//! Update operation and its helper.

use anyhow::{Context as _, Result, anyhow};
use rusqlite::params_from_iter;
use std::collections::HashMap;

use super::super::{LocaleContext, coerce_value, locale_write_column, read::find_by_id_raw};
use crate::core::{
    CollectionDefinition, Document,
    field::{FieldDefinition, FieldType},
};

/// Update a document by ID. Returns the updated document.
pub fn update(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string();

    let mut set_clauses = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    collect_update_params(
        &def.fields,
        data,
        &locale_ctx,
        &mut set_clauses,
        &mut params,
        &mut idx,
        "",
    );

    if def.timestamps {
        set_clauses.push(format!("updated_at = ?{}", idx));
        params.push(Box::new(now));
        idx += 1;
    }

    if set_clauses.is_empty() {
        return find_by_id_raw(conn, slug, def, id, locale_ctx)?
            .ok_or_else(|| anyhow!("Document not found"));
    }

    let sql = format!(
        "UPDATE {} SET {} WHERE id = ?{}",
        slug,
        set_clauses.join(", "),
        idx
    );
    params.push(Box::new(id.to_string()));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    conn.execute(&sql, params_from_iter(param_refs.iter()))
        .with_context(|| format!("Failed to update document {} in '{}'", id, slug))?;

    find_by_id_raw(conn, slug, def, id, locale_ctx)?
        .ok_or_else(|| anyhow!("Document not found after update"))
}

/// Recursively collect SET clauses + params for UPDATE.
/// Handles arbitrary nesting: Group (prefixed), Row/Collapsible/Tabs (promoted flat).
pub(super) fn collect_update_params(
    fields: &[FieldDefinition],
    data: &HashMap<String, String>,
    locale_ctx: &Option<&LocaleContext>,
    set_clauses: &mut Vec<String>,
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    idx: &mut usize,
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
                    set_clauses,
                    params,
                    idx,
                    &new_prefix,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_update_params(
                    &field.fields,
                    data,
                    locale_ctx,
                    set_clauses,
                    params,
                    idx,
                    prefix,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_update_params(
                        &tab.fields,
                        data,
                        locale_ctx,
                        set_clauses,
                        params,
                        idx,
                        prefix,
                    );
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
                    set_clauses.push(format!("{} = ?{}", col_name, *idx));
                    params.push(coerce_value(&field.field_type, value));
                    *idx += 1;
                } else if field.field_type == FieldType::Checkbox {
                    set_clauses.push(format!("{} = ?{}", col_name, *idx));
                    params.push(Box::new(0i32));
                    *idx += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::create::create;
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::*;
    use rusqlite::Connection;

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Text).build(),
        ];
        def
    }

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();
        conn
    }

    #[test]
    fn update_basic() {
        let conn = setup_db();
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
        let conn = setup_db();
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
        let conn = setup_db();
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Something".to_string());

        let result = update(&conn, "posts", &def, "nonexistent-id", &data, None);
        assert!(result.is_err(), "Updating non-existent ID should error");
    }

    #[test]
    fn update_with_group_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                meta__color TEXT,
                meta__size TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, meta__color, meta__size, created_at, updated_at)
            VALUES ('p1', 'blue', 'small', '2024-01-01', '2024-01-01');",
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
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                name TEXT
            );
            INSERT INTO events (id, name) VALUES ('e1', 'Original');",
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
        let conn = setup_db();
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
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                social__github TEXT,
                social__twitter TEXT,
                body TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, social__github, social__twitter, body, created_at, updated_at)
            VALUES ('p1', 'https://github.com', '@old', 'Old body', '2024-01-01', '2024-01-01');",
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
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                meta__title TEXT,
                meta__slug TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, meta__title, meta__slug, created_at, updated_at)
            VALUES ('abc', 'Old', 'old', '2024-01-01', '2024-01-01');",
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
}
