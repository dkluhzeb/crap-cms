//! Document hydration and join table save orchestration.

mod group;
mod locale;
mod read;
pub(crate) mod save;

pub use read::{hydrate_document, hydrate_documents};
pub use save::save_join_table_data;
pub(crate) use save::{parse_id_list, parse_polymorphic_values};

#[cfg(test)]
pub(super) mod test_helpers {
    use crate::config::CrapConfig;
    use crate::core::{collection::*, field::*};
    use crate::db::{BoxedConnection, DbConnection, pool};
    use tempfile::TempDir;

    pub fn setup_join_db() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            -- Has-many junction table
            CREATE TABLE posts_tags (
                parent_id TEXT,
                related_id TEXT,
                _order INTEGER
            );
            -- Array join table
            CREATE TABLE posts_items (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                label TEXT,
                value TEXT
            );
            -- Blocks join table
            CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT
            );
            INSERT INTO posts (id, title, created_at, updated_at) VALUES ('p1', 'Post 1', '2024-01-01', '2024-01-01');",
        ).unwrap();
        (dir, conn)
    }

    pub fn array_sub_fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("label", FieldType::Text).build(),
            FieldDefinition::builder("value", FieldType::Text).build(),
        ]
    }

    pub fn posts_def_with_joins() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(array_sub_fields())
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks).build(),
        ];
        def
    }
}

#[cfg(test)]
mod tests;
