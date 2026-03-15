//! Collection table sync: create and alter collection tables from Lua definitions.

mod alter;
mod create;
mod indexes;

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::CollectionDefinition,
    db::{DbConnection, query::fts},
};

use crate::db::migrate::helpers::{sync_join_tables, sync_versions_table, table_exists};

#[cfg(test)]
pub(super) use create::create_collection_table;

pub(super) fn sync_collection_table(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table_exists = table_exists(conn, slug)?;

    if !table_exists {
        create::create_collection_table(conn, slug, def, locale_config)?;
    } else {
        alter::alter_collection_table(conn, slug, def, locale_config)?;
    }

    // Sync join tables for has-many relationships and array fields
    sync_join_tables(conn, slug, &def.fields, locale_config)?;

    // Sync versions table if versioning is enabled
    if def.has_versions() {
        sync_versions_table(conn, slug)?;
    }

    // Sync FTS5 full-text search index
    if conn.supports_fts() {
        fts::sync_fts_table(conn, slug, def, locale_config)?;
    }

    // Sync B-tree indexes (field-level index=true + collection-level compound indexes)
    indexes::sync_indexes(conn, slug, def, locale_config)?;

    Ok(())
}

#[cfg(test)]
pub(super) mod test_helpers {
    use crate::config::{CrapConfig, LocaleConfig};
    use crate::core::collection::*;
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

#[cfg(test)]
mod tests {
    use super::test_helpers::*;
    use super::*;
    use crate::core::collection::*;
    use crate::db::migrate::helpers::{get_table_columns, table_exists};

    #[test]
    fn versioned_collection_creates_versions_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.versions = Some(VersionsConfig::new(true, 10));
        sync_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        assert!(table_exists(&conn, "_versions_posts").unwrap());
        let cols = get_table_columns(&conn, "_versions_posts").unwrap();
        assert!(cols.contains("_parent"));
        assert!(cols.contains("_version"));
        assert!(cols.contains("_status"));
        assert!(cols.contains("_latest"));
        assert!(cols.contains("snapshot"));
    }
}
