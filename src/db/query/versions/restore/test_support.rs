//! Shared fixtures for the restore tests.

use tempfile::TempDir;

use crate::{
    config::{CrapConfig, LocaleConfig},
    core::{CollectionDefinition, FieldAdmin, FieldDefinition, FieldType, VersionsConfig},
    db::{BoxedConnection, pool},
};

pub(super) fn setup_conn() -> (TempDir, BoxedConnection) {
    let dir = TempDir::new().unwrap();
    let config = CrapConfig::default();
    let db_pool = pool::create_pool(dir.path(), &config).unwrap();
    let conn = db_pool.get().unwrap();
    (dir, conn)
}

pub(super) const VERSIONS_SNIPPETS_DDL: &str = "CREATE TABLE _versions_snippets (
    id TEXT PRIMARY KEY,
    _parent TEXT NOT NULL,
    _version INTEGER NOT NULL,
    _status TEXT NOT NULL,
    _latest INTEGER NOT NULL DEFAULT 0,
    snapshot TEXT NOT NULL,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);";

pub(super) fn code_lang_def(localized: bool) -> CollectionDefinition {
    let mut def = CollectionDefinition::new("snippets");
    def.fields = vec![
        FieldDefinition::builder("snippet", FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .languages(vec!["javascript".to_string(), "python".to_string()])
                    .build(),
            )
            .localized(localized)
            .build(),
    ];
    def.versions = Some(VersionsConfig::new(true, 10));
    def
}

pub(super) fn en_de() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}
