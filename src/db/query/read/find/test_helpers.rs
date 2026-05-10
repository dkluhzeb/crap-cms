//! Shared test fixtures for find/ submodules.

use tempfile::TempDir;

use crate::config::{CrapConfig, DatabaseConfig};
use crate::core::CollectionDefinition;
use crate::core::{FieldDefinition, FieldType};
use crate::db::{DbConnection, DbPool, pool};

pub(super) fn test_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("status", FieldType::Text).build(),
    ];
    def
}

pub(super) fn setup_db() -> (TempDir, DbPool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = CrapConfig {
        database: DatabaseConfig {
            path: "test.db".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
    db_pool
        .get()
        .unwrap()
        .execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();
    (tmp, db_pool)
}
