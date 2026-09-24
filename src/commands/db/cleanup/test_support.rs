//! Fixtures shared by the cleanup submodules' unit tests.

use tempfile::TempDir;

use crate::{
    config::{CrapConfig, LocaleConfig},
    core::{CollectionDefinition, FieldDefinition, FieldType},
    db::{BoxedConnection, pool},
};

pub(super) fn no_locale() -> LocaleConfig {
    LocaleConfig::default()
}

pub(super) fn locale_en_de() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

pub(super) fn simple_collection(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.timestamps = true;
    def.fields = fields;

    def
}

pub(super) fn text_field(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Text).build()
}

pub(super) fn localized_array(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Array)
        .localized(true)
        .fields(vec![text_field("label")])
        .build()
}

pub(super) fn make_conn() -> (TempDir, BoxedConnection) {
    let dir = TempDir::new().unwrap();
    let cfg = CrapConfig::default();
    let p = pool::create_pool(dir.path(), &cfg).unwrap();
    let conn = p.get().unwrap();

    (dir, conn)
}
