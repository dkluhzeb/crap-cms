//! Collection table sync orchestrator.

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::CollectionDefinition,
    db::{DbConnection, query::fts},
};

use crate::db::migrate::helpers::{sync_join_tables, sync_versions_table, table_exists};

use super::{alter, create, indexes};

/// Sync a collection's schema: create or alter table, join tables, versions, FTS, and indexes.
pub(in crate::db::migrate) fn sync_collection_table(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    if table_exists(conn, slug)? {
        alter::alter_collection_table(conn, slug, def, locale_config)?;
    } else {
        create::create_collection_table(conn, slug, def, locale_config)?;
    }

    sync_join_tables(conn, slug, &def.fields, locale_config)?;

    if def.has_versions() {
        sync_versions_table(conn, slug)?;
    }

    if conn.supports_fts() {
        fts::sync_fts_table(conn, slug, def, locale_config)?;
    }

    indexes::sync_indexes(conn, slug, def, locale_config)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::*;
    use super::*;
    use crate::core::collection::VersionsConfig;
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
