//! Shared fixtures for the create tests.

use tempfile::TempDir;

use crate::{
    config::CrapConfig,
    core::{CollectionDefinition, FieldDefinition, FieldType},
    db::{BoxedConnection, DbConnection, pool},
};

pub(super) fn setup_db(ddl: &str) -> (TempDir, BoxedConnection) {
    let dir = TempDir::new().unwrap();
    let config = CrapConfig::default();
    let p = pool::create_pool(dir.path(), &config).unwrap();
    let conn = p.get().unwrap();
    conn.execute_batch(ddl).unwrap();
    (dir, conn)
}

pub(super) fn posts_ddl() -> &'static str {
    "CREATE TABLE posts (
        id TEXT PRIMARY KEY,
        _revision INTEGER NOT NULL DEFAULT 0,
        title TEXT,
        status TEXT,
        created_at TEXT,
        updated_at TEXT
    )"
}

pub(super) fn test_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("status", FieldType::Text).build(),
    ];
    def
}
