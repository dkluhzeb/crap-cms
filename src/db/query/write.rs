//! Write operations: create, update, delete.

use anyhow::{Context as _, Result};
use rusqlite::params_from_iter;
use std::collections::HashMap;

use crate::core::{CollectionDefinition, Document};
use crate::core::field::FieldType;
use super::{LocaleContext, locale_write_column, coerce_value};
use super::read::find_by_id_raw;

/// Create a new document. Returns the created document.
pub fn create(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let id = nanoid::nanoid!();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z").to_string();

    let mut columns = vec!["id".to_string()];
    let mut placeholders = vec!["?1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(id.clone())];
    let mut idx = 2;

    collect_insert_params(&def.fields, data, &locale_ctx, &mut columns, &mut placeholders, &mut params, &mut idx, "");

    if def.timestamps {
        columns.push("created_at".to_string());
        placeholders.push(format!("?{}", idx));
        params.push(Box::new(now.clone()));
        idx += 1;

        columns.push("updated_at".to_string());
        placeholders.push(format!("?{}", idx));
        params.push(Box::new(now));
    }

    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        slug,
        columns.join(", "),
        placeholders.join(", ")
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    conn.execute(&sql, params_from_iter(param_refs.iter()))
        .with_context(|| format!("Failed to insert into '{}'", slug))?;

    // Return the created document with the same locale context
    find_by_id_raw(conn, slug, def, &id, locale_ctx)?
        .ok_or_else(|| anyhow::anyhow!("Failed to find newly created document"))
}

/// Update a document by ID. Returns the updated document.
pub fn update(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z").to_string();

    let mut set_clauses = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    collect_update_params(&def.fields, data, &locale_ctx, &mut set_clauses, &mut params, &mut idx, "");

    if def.timestamps {
        set_clauses.push(format!("updated_at = ?{}", idx));
        params.push(Box::new(now));
        idx += 1;
    }

    if set_clauses.is_empty() {
        return find_by_id_raw(conn, slug, def, id, locale_ctx)?
            .ok_or_else(|| anyhow::anyhow!("Document not found"));
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
        .ok_or_else(|| anyhow::anyhow!("Document not found after update"))
}

/// Recursively collect columns, placeholders, and params for INSERT.
/// Handles arbitrary nesting: Group (prefixed), Row/Collapsible/Tabs (promoted flat).
fn collect_insert_params(
    fields: &[crate::core::field::FieldDefinition],
    data: &HashMap<String, String>,
    locale_ctx: &Option<&LocaleContext>,
    columns: &mut Vec<String>,
    placeholders: &mut Vec<String>,
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
                collect_insert_params(&field.fields, data, locale_ctx, columns, placeholders, params, idx, &new_prefix);
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_insert_params(&field.fields, data, locale_ctx, columns, placeholders, params, idx, prefix);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_insert_params(&tab.fields, data, locale_ctx, columns, placeholders, params, idx, prefix);
                }
            }
            _ => {
                if !field.has_parent_column() { continue; }
                let data_key = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                let col_name = locale_write_column(&data_key, field, locale_ctx);
                if let Some(value) = data.get(&data_key) {
                    columns.push(col_name);
                    placeholders.push(format!("?{}", *idx));
                    params.push(coerce_value(&field.field_type, value));
                    *idx += 1;
                } else if field.field_type == FieldType::Checkbox {
                    columns.push(col_name);
                    placeholders.push(format!("?{}", *idx));
                    params.push(Box::new(0i32));
                    *idx += 1;
                }
            }
        }
    }
}

/// Recursively collect SET clauses + params for UPDATE.
/// Handles arbitrary nesting: Group (prefixed), Row/Collapsible/Tabs (promoted flat).
fn collect_update_params(
    fields: &[crate::core::field::FieldDefinition],
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
                collect_update_params(&field.fields, data, locale_ctx, set_clauses, params, idx, &new_prefix);
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_update_params(&field.fields, data, locale_ctx, set_clauses, params, idx, prefix);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_update_params(&tab.fields, data, locale_ctx, set_clauses, params, idx, prefix);
                }
            }
            _ => {
                if !field.has_parent_column() { continue; }
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

/// Delete a document by ID.
pub fn delete(conn: &rusqlite::Connection, slug: &str, id: &str) -> Result<()> {
    let sql = format!("DELETE FROM {} WHERE id = ?1", slug);
    conn.execute(&sql, [id])
        .with_context(|| format!("Failed to delete document {} from '{}'", id, slug))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::find_by_id_raw;
    use rusqlite::Connection;
    use crate::core::collection::*;
    use crate::core::field::*;

    fn test_def() -> CollectionDefinition {
        CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "title".to_string(),
                    ..Default::default()
                },
                FieldDefinition {
                    name: "status".to_string(),
                    ..Default::default()
                },
            ],
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

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();
        conn
    }

    #[test]
    fn create_basic() {
        let conn = setup_db();
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Hello World".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert!(!doc.id.is_empty());
        assert_eq!(doc.get_str("title"), Some("Hello World"));
    }

    #[test]
    fn create_with_timestamps() {
        let conn = setup_db();
        let def = test_def();
        let data = HashMap::new();

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert!(doc.created_at.is_some(), "created_at should be set");
        assert!(doc.updated_at.is_some(), "updated_at should be set");
        // Both should be the same on creation
        assert_eq!(doc.created_at, doc.updated_at);
    }

    #[test]
    fn create_checkbox_defaults_to_zero() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT,
                published INTEGER,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();

        let mut def = test_def();
        def.fields.push(FieldDefinition {
            name: "published".to_string(),
            field_type: FieldType::Checkbox,
            ..Default::default()
        });

        // Create without providing the checkbox field
        let data = HashMap::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();

        // Checkbox should default to 0 (integer)
        let published = doc.get("published").unwrap();
        assert_eq!(published, &serde_json::json!(0));
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
        assert_eq!(updated.get_str("status"), Some("draft"), "status should be preserved");
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
    fn delete_basic() {
        let conn = setup_db();
        let def = test_def();
        let data = HashMap::new();

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let id = doc.id.clone();

        delete(&conn, "posts", &id).unwrap();

        let found = find_by_id_raw(&conn, "posts", &def, &id, None).unwrap();
        assert!(found.is_none(), "Document should be gone after delete");
    }

    #[test]
    fn delete_nonexistent() {
        let conn = setup_db();
        // Deleting a non-existent ID should not error (0 rows affected)
        let result = delete(&conn, "posts", "does-not-exist");
        assert!(result.is_ok(), "Deleting non-existent ID should not error");
    }

    #[test]
    fn create_with_group_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                meta__color TEXT,
                meta__size TEXT,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "title".to_string(),
                    ..Default::default()
                },
                FieldDefinition {
                    name: "meta".to_string(),
                    field_type: FieldType::Group,
                    fields: vec![
                        FieldDefinition {
                            name: "color".to_string(),
                            ..Default::default()
                        },
                        FieldDefinition {
                            name: "size".to_string(),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        let mut data = HashMap::new();
        data.insert("title".to_string(), "Post1".to_string());
        data.insert("meta__color".to_string(), "red".to_string());
        data.insert("meta__size".to_string(), "large".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("title"), Some("Post1"));
        // Group sub-fields stored as prefixed columns
        assert_eq!(doc.get_str("meta__color"), Some("red"));
        assert_eq!(doc.get_str("meta__size"), Some("large"));
    }

    #[test]
    fn create_without_timestamps() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                name TEXT
            )"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "events".to_string(),
            labels: CollectionLabels::default(),
            timestamps: false,
            fields: vec![
                FieldDefinition {
                    name: "name".to_string(),
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        let mut data = HashMap::new();
        data.insert("name".to_string(), "Event1".to_string());

        let doc = create(&conn, "events", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("name"), Some("Event1"));
        assert!(doc.created_at.is_none(), "no timestamps collection should have no created_at");
        assert!(doc.updated_at.is_none(), "no timestamps collection should have no updated_at");
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
            VALUES ('p1', 'blue', 'small', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "meta".to_string(),
                    field_type: FieldType::Group,
                    fields: vec![
                        FieldDefinition {
                            name: "color".to_string(),
                            ..Default::default()
                        },
                        FieldDefinition {
                            name: "size".to_string(),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

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
            INSERT INTO events (id, name) VALUES ('e1', 'Original');"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "events".to_string(),
            labels: CollectionLabels::default(),
            timestamps: false,
            fields: vec![
                FieldDefinition {
                    name: "name".to_string(),
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

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
        let no_ts_def = CollectionDefinition {
            timestamps: false,
            ..test_def()
        };
        let empty_data = HashMap::new();
        let result = update(&conn, "posts", &no_ts_def, &id, &empty_data, None).unwrap();
        assert_eq!(result.get_str("title"), Some("MyTitle"));
    }

    #[test]
    fn create_group_with_checkbox_sub_field_default() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                settings__featured INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "settings".to_string(),
                    field_type: FieldType::Group,
                    fields: vec![
                        FieldDefinition {
                            name: "featured".to_string(),
                            field_type: FieldType::Checkbox,
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        // Create without providing the checkbox group sub-field — should default to 0
        let data = HashMap::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let val = doc.get("settings__featured").unwrap();
        assert_eq!(val, &serde_json::json!(0));
    }

    // ── create/update with collapsible fields ───────────────────────────

    #[test]
    fn create_with_collapsible_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                notes TEXT,
                footer TEXT,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "extra".to_string(),
                    field_type: FieldType::Collapsible,
                    fields: vec![
                        FieldDefinition { name: "notes".to_string(), ..Default::default() },
                        FieldDefinition { name: "footer".to_string(), ..Default::default() },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        let mut data = HashMap::new();
        data.insert("notes".to_string(), "Some notes".to_string());
        data.insert("footer".to_string(), "Copyright".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("notes"), Some("Some notes"));
        assert_eq!(doc.get_str("footer"), Some("Copyright"));
    }

    // ── create/update with tabs fields ──────────────────────────────────

    #[test]
    fn create_with_tabs_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                body TEXT,
                slug TEXT,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "layout".to_string(),
                    field_type: FieldType::Tabs,
                    tabs: vec![
                        FieldTab { label: "Content".to_string(), description: None, fields: vec![
                            FieldDefinition { name: "body".to_string(), ..Default::default() },
                        ]},
                        FieldTab { label: "Meta".to_string(), description: None, fields: vec![
                            FieldDefinition { name: "slug".to_string(), ..Default::default() },
                        ]},
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        let mut data = HashMap::new();
        data.insert("body".to_string(), "Hello world".to_string());
        data.insert("slug".to_string(), "hello-world".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("body"), Some("Hello world"));
        assert_eq!(doc.get_str("slug"), Some("hello-world"));
    }

    // ── create with tabs containing group (the regression case) ─────────

    #[test]
    fn create_with_tabs_containing_group() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                social__github TEXT,
                social__twitter TEXT,
                body TEXT,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "layout".to_string(),
                    field_type: FieldType::Tabs,
                    tabs: vec![
                        FieldTab {
                            label: "Social".to_string(),
                            description: None,
                            fields: vec![
                                FieldDefinition {
                                    name: "social".to_string(),
                                    field_type: FieldType::Group,
                                    fields: vec![
                                        FieldDefinition { name: "github".to_string(), ..Default::default() },
                                        FieldDefinition { name: "twitter".to_string(), ..Default::default() },
                                    ],
                                    ..Default::default()
                                },
                            ],
                        },
                        FieldTab {
                            label: "Content".to_string(),
                            description: None,
                            fields: vec![
                                FieldDefinition { name: "body".to_string(), ..Default::default() },
                            ],
                        },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        let mut data = HashMap::new();
        data.insert("social__github".to_string(), "https://github.com".to_string());
        data.insert("social__twitter".to_string(), "@test".to_string());
        data.insert("body".to_string(), "Content here".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("social__github"), Some("https://github.com"));
        assert_eq!(doc.get_str("social__twitter"), Some("@test"));
        assert_eq!(doc.get_str("body"), Some("Content here"));
    }

    // ── update with tabs containing group ───────────────────────────────

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
            VALUES ('p1', 'https://github.com', '@old', 'Old body', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "layout".to_string(),
                    field_type: FieldType::Tabs,
                    tabs: vec![
                        FieldTab {
                            label: "Social".to_string(),
                            description: None,
                            fields: vec![
                                FieldDefinition {
                                    name: "social".to_string(),
                                    field_type: FieldType::Group,
                                    fields: vec![
                                        FieldDefinition { name: "github".to_string(), ..Default::default() },
                                        FieldDefinition { name: "twitter".to_string(), ..Default::default() },
                                    ],
                                    ..Default::default()
                                },
                            ],
                        },
                        FieldTab {
                            label: "Content".to_string(),
                            description: None,
                            fields: vec![
                                FieldDefinition { name: "body".to_string(), ..Default::default() },
                            ],
                        },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        // Only update twitter, leave github and body untouched
        let mut data = HashMap::new();
        data.insert("social__twitter".to_string(), "@new".to_string());

        let doc = update(&conn, "posts", &def, "p1", &data, None).unwrap();
        assert_eq!(doc.get_str("social__twitter"), Some("@new"));
        assert_eq!(doc.get_str("social__github"), Some("https://github.com"), "github should be preserved");
        assert_eq!(doc.get_str("body"), Some("Old body"), "body should be preserved");
    }

    // ── deeply nested: tabs → collapsible → group ───────────────────────

    #[test]
    fn create_deeply_nested_tabs_collapsible_group() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                og__image TEXT,
                canonical TEXT,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "layout".to_string(),
                    field_type: FieldType::Tabs,
                    tabs: vec![
                        FieldTab {
                            label: "Advanced".to_string(),
                            description: None,
                            fields: vec![
                                FieldDefinition {
                                    name: "advanced".to_string(),
                                    field_type: FieldType::Collapsible,
                                    fields: vec![
                                        FieldDefinition {
                                            name: "og".to_string(),
                                            field_type: FieldType::Group,
                                            fields: vec![
                                                FieldDefinition { name: "image".to_string(), ..Default::default() },
                                            ],
                                            ..Default::default()
                                        },
                                        FieldDefinition { name: "canonical".to_string(), ..Default::default() },
                                    ],
                                    ..Default::default()
                                },
                            ],
                        },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        let mut data = HashMap::new();
        data.insert("og__image".to_string(), "hero.jpg".to_string());
        data.insert("canonical".to_string(), "https://example.com".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("og__image"), Some("hero.jpg"));
        assert_eq!(doc.get_str("canonical"), Some("https://example.com"));
    }

    // ── Group containing layout fields (the former terminal-node bug) ─────

    #[test]
    fn create_group_containing_row() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                meta__title TEXT,
                meta__slug TEXT,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "meta".to_string(),
                    field_type: FieldType::Group,
                    fields: vec![
                        FieldDefinition {
                            name: "r".to_string(),
                            field_type: FieldType::Row,
                            fields: vec![
                                FieldDefinition { name: "title".to_string(), ..Default::default() },
                                FieldDefinition { name: "slug".to_string(), ..Default::default() },
                            ],
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        let mut data = HashMap::new();
        data.insert("meta__title".to_string(), "Hello".to_string());
        data.insert("meta__slug".to_string(), "hello".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("meta__title"), Some("Hello"));
        assert_eq!(doc.get_str("meta__slug"), Some("hello"));
    }

    #[test]
    fn create_group_containing_tabs() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                settings__theme TEXT,
                settings__cache_ttl TEXT,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "settings".to_string(),
                    field_type: FieldType::Group,
                    fields: vec![
                        FieldDefinition {
                            name: "layout".to_string(),
                            field_type: FieldType::Tabs,
                            tabs: vec![
                                FieldTab {
                                    label: "General".to_string(),
                                    description: None,
                                    fields: vec![
                                        FieldDefinition { name: "theme".to_string(), ..Default::default() },
                                    ],
                                },
                                FieldTab {
                                    label: "Advanced".to_string(),
                                    description: None,
                                    fields: vec![
                                        FieldDefinition { name: "cache_ttl".to_string(), ..Default::default() },
                                    ],
                                },
                            ],
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        let mut data = HashMap::new();
        data.insert("settings__theme".to_string(), "dark".to_string());
        data.insert("settings__cache_ttl".to_string(), "3600".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("settings__theme"), Some("dark"));
        assert_eq!(doc.get_str("settings__cache_ttl"), Some("3600"));
    }

    #[test]
    fn create_group_tabs_group_three_levels() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                outer__inner__deep TEXT,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "outer".to_string(),
                    field_type: FieldType::Group,
                    fields: vec![
                        FieldDefinition {
                            name: "t".to_string(),
                            field_type: FieldType::Tabs,
                            tabs: vec![FieldTab {
                                label: "Tab".to_string(),
                                description: None,
                                fields: vec![
                                    FieldDefinition {
                                        name: "inner".to_string(),
                                        field_type: FieldType::Group,
                                        fields: vec![
                                            FieldDefinition { name: "deep".to_string(), ..Default::default() },
                                        ],
                                        ..Default::default()
                                    },
                                ],
                            }],
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        let mut data = HashMap::new();
        data.insert("outer__inner__deep".to_string(), "bottom".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("outer__inner__deep"), Some("bottom"));
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
            VALUES ('abc', 'Old', 'old', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "meta".to_string(),
                    field_type: FieldType::Group,
                    fields: vec![
                        FieldDefinition {
                            name: "r".to_string(),
                            field_type: FieldType::Row,
                            fields: vec![
                                FieldDefinition { name: "title".to_string(), ..Default::default() },
                                FieldDefinition { name: "slug".to_string(), ..Default::default() },
                            ],
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        let mut data = HashMap::new();
        data.insert("meta__title".to_string(), "New Title".to_string());

        let doc = update(&conn, "posts", &def, "abc", &data, None).unwrap();
        assert_eq!(doc.get_str("meta__title"), Some("New Title"));
        assert_eq!(doc.get_str("meta__slug"), Some("old"), "Unset field should be preserved");
    }
}
