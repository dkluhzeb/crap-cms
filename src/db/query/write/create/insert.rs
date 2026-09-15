//! The create entry point: INSERT one document row and read it back.

use anyhow::{Context as _, Result, anyhow};
use nanoid::nanoid;

use crate::{
    core::{CollectionDefinition, Document, DocumentFields},
    db::{
        DbConnection, DbValue, LocaleContext,
        query::{
            helpers::utc_now,
            read::find_by_id_raw,
            write::create::collector::{InsertCollector, collect_insert_params},
        },
    },
};

/// Create a new document. Returns the created document.
///
/// # Errors
///
/// Returns a backend error if the INSERT, join-table writes, or re-read fails.
pub fn create(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    data: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let id = nanoid!();
    let now = utc_now();

    let mut collector = InsertCollector::new(conn, &id);

    collect_insert_params(&def.fields, data, locale_ctx, &mut collector, conn)?;

    if def.timestamps {
        collector.push(conn, "created_at", DbValue::Text(now.clone()));
        collector.push(conn, "updated_at", DbValue::Text(now));
    }

    let sql = format!(
        "INSERT INTO \"{}\" ({}) VALUES ({})",
        slug,
        collector.columns.join(", "),
        collector.placeholders.join(", ")
    );

    conn.execute(&sql, &collector.params)
        .with_context(|| format!("Failed to insert into '{slug}'"))?;

    // Return the created document with the same locale context.
    find_by_id_raw(conn, slug, def, &id, locale_ctx, false)?
        .ok_or_else(|| anyhow!("Failed to find newly created document"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::create;
    use crate::{
        core::{CollectionDefinition, DocumentFields, FieldDefinition, FieldType},
        db::query::write::create::test_support::{posts_ddl, setup_db, test_def},
    };

    #[test]
    fn create_basic() {
        let (_dir, conn) = setup_db(posts_ddl());
        let def = test_def();
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello World"));

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert!(!doc.id.is_empty());
        assert_eq!(doc.get_str("title"), Some("Hello World"));
    }

    #[test]
    fn create_with_timestamps() {
        let (_dir, conn) = setup_db(posts_ddl());
        let def = test_def();
        let data = DocumentFields::new();

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert!(doc.created_at.is_some(), "created_at should be set");
        assert!(doc.updated_at.is_some(), "updated_at should be set");
        // Both should be the same on creation
        assert_eq!(doc.created_at, doc.updated_at);
    }

    #[test]
    fn create_without_timestamps() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                name TEXT
            )",
        );

        let mut def = CollectionDefinition::new("events");
        def.timestamps = false;
        def.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
        let def = def;

        let mut data = DocumentFields::new();
        data.insert("name".to_string(), json!("Event1"));

        let doc = create(&conn, "events", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("name"), Some("Event1"));
        assert!(
            doc.created_at.is_none(),
            "no timestamps collection should have no created_at"
        );
        assert!(
            doc.updated_at.is_none(),
            "no timestamps collection should have no updated_at"
        );
    }
}
