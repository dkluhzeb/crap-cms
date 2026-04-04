//! Collection table sync: create and alter collection tables from Lua definitions.

mod alter;
mod create;
mod indexes;
mod sync;

pub(super) use create::append_default_value_for;
pub(super) use sync::sync_collection_table;

#[cfg(test)]
pub(super) use create::create_collection_table;

#[cfg(test)]
pub(super) mod test_helpers {
    use crate::config::{CrapConfig, LocaleConfig};
    use crate::core::CollectionDefinition;
    use crate::core::field::{FieldDefinition, FieldType};
    use crate::db::{DbPool, pool};
    use tempfile::TempDir;

    pub fn in_memory_pool() -> (TempDir, DbPool) {
        let dir = TempDir::new().expect("temp dir");
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).expect("in-memory pool");
        (dir, p)
    }

    pub fn no_locale() -> LocaleConfig {
        LocaleConfig::default()
    }

    pub fn locale_en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    pub fn simple_collection(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.fields = fields;
        def
    }

    pub fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    pub fn localized_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .localized(true)
            .build()
    }
}
