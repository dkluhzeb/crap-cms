//! Write operations: create, update, delete.

use anyhow::{Context, Result};
use rusqlite::params_from_iter;
use std::collections::HashMap;

use crate::core::{CollectionDefinition, Document};
use crate::core::field::FieldType;
use super::{LocaleContext, locale_write_column, coerce_value};
use super::read::find_by_id;

/// Create a new document. Returns the created document.
pub fn create(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let id = nanoid::nanoid!();
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut columns = vec!["id".to_string()];
    let mut placeholders = vec!["?1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(id.clone())];
    let mut idx = 2;

    for field in &def.fields {
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                let data_key = format!("{}__{}", field.name, sub.name);
                let col_name = locale_write_column(&data_key, sub, &locale_ctx);
                if let Some(value) = data.get(&data_key) {
                    columns.push(col_name);
                    placeholders.push(format!("?{}", idx));
                    params.push(coerce_value(&sub.field_type, value));
                    idx += 1;
                } else if sub.field_type == FieldType::Checkbox {
                    columns.push(col_name);
                    placeholders.push(format!("?{}", idx));
                    params.push(Box::new(0i32));
                    idx += 1;
                }
            }
            continue;
        }
        if !field.has_parent_column() {
            continue;
        }
        let col_name = locale_write_column(&field.name, field, &locale_ctx);
        if let Some(value) = data.get(&field.name) {
            columns.push(col_name);
            placeholders.push(format!("?{}", idx));
            params.push(coerce_value(&field.field_type, value));
            idx += 1;
        } else if field.field_type == FieldType::Checkbox {
            columns.push(col_name);
            placeholders.push(format!("?{}", idx));
            params.push(Box::new(0i32));
            idx += 1;
        }
    }

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
    find_by_id(conn, slug, def, &id, locale_ctx)?
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
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut set_clauses = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    for field in &def.fields {
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                let data_key = format!("{}__{}", field.name, sub.name);
                let col_name = locale_write_column(&data_key, sub, &locale_ctx);
                if let Some(value) = data.get(&data_key) {
                    set_clauses.push(format!("{} = ?{}", col_name, idx));
                    params.push(coerce_value(&sub.field_type, value));
                    idx += 1;
                } else if sub.field_type == FieldType::Checkbox {
                    set_clauses.push(format!("{} = ?{}", col_name, idx));
                    params.push(Box::new(0i32));
                    idx += 1;
                }
            }
            continue;
        }
        if !field.has_parent_column() {
            continue;
        }
        let col_name = locale_write_column(&field.name, field, &locale_ctx);
        if let Some(value) = data.get(&field.name) {
            set_clauses.push(format!("{} = ?{}", col_name, idx));
            params.push(coerce_value(&field.field_type, value));
            idx += 1;
        } else if field.field_type == FieldType::Checkbox {
            set_clauses.push(format!("{} = ?{}", col_name, idx));
            params.push(Box::new(0i32));
            idx += 1;
        }
    }

    if def.timestamps {
        set_clauses.push(format!("updated_at = ?{}", idx));
        params.push(Box::new(now));
        idx += 1;
    }

    if set_clauses.is_empty() {
        return find_by_id(conn, slug, def, id, locale_ctx)?
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

    find_by_id(conn, slug, def, id, locale_ctx)?
        .ok_or_else(|| anyhow::anyhow!("Document not found after update"))
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
    use super::find_by_id;
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
                    field_type: FieldType::Text,
                    required: false,
                    unique: false,
                    validate: None,
                    default_value: None,
                    options: vec![],
                    admin: FieldAdmin::default(),
                    hooks: FieldHooks::default(),
                    access: FieldAccess::default(),
                    relationship: None,
                    fields: vec![],
                    blocks: vec![],
                    localized: false,
                },
                FieldDefinition {
                    name: "status".to_string(),
                    field_type: FieldType::Text,
                    required: false,
                    unique: false,
                    validate: None,
                    default_value: None,
                    options: vec![],
                    admin: FieldAdmin::default(),
                    hooks: FieldHooks::default(),
                    access: FieldAccess::default(),
                    relationship: None,
                    fields: vec![],
                    blocks: vec![],
                    localized: false,
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
        versions: None,
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
            required: false,
            unique: false,
            validate: None,
            default_value: None,
            options: vec![],
            admin: FieldAdmin::default(),
            hooks: FieldHooks::default(),
            access: FieldAccess::default(),
            relationship: None,
            fields: vec![],
            blocks: vec![],
            localized: false,
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

        let found = find_by_id(&conn, "posts", &def, &id, None).unwrap();
        assert!(found.is_none(), "Document should be gone after delete");
    }

    #[test]
    fn delete_nonexistent() {
        let conn = setup_db();
        // Deleting a non-existent ID should not error (0 rows affected)
        let result = delete(&conn, "posts", "does-not-exist");
        assert!(result.is_ok(), "Deleting non-existent ID should not error");
    }
}
