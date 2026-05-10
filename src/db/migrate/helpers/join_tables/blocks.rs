//! Blocks-field join tables: create + ensure locale column.

use anyhow::{Context as _, Result};
use tracing::info;

use crate::config::LocaleConfig;
use crate::db::DbConnection;
use crate::db::migrate::helpers::column_specs::ensure_locale_column;
use crate::db::migrate::helpers::introspection::{sanitize_locale, table_exists};
use crate::db::query::helpers::join_table;

/// Sync a blocks join table (create or ensure locale column).
pub(super) fn sync_blocks_table(
    conn: &dyn DbConnection,
    collection_slug: &str,
    full_name: &str,
    has_locale_col: bool,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table_name = join_table(collection_slug, full_name);

    if !table_exists(conn, &table_name)? {
        let locale_col = if has_locale_col {
            let default_loc = sanitize_locale(&locale_config.default_locale)?;
            format!(", _locale TEXT NOT NULL DEFAULT '{}'", default_loc)
        } else {
            String::new()
        };

        let sql = format!(
            "CREATE TABLE \"{}\" (\
                id TEXT PRIMARY KEY, \
                parent_id TEXT NOT NULL REFERENCES \"{}\"(id) ON DELETE CASCADE, \
                _order INTEGER NOT NULL DEFAULT 0, \
                _block_type TEXT NOT NULL, \
                data TEXT NOT NULL DEFAULT '{{}}'\
                {}\
            )",
            table_name, collection_slug, locale_col
        );

        info!("Creating blocks table: {}", table_name);
        conn.execute_ddl(&sql, &[])
            .with_context(|| format!("Failed to create blocks table {}", table_name))?;
    } else if has_locale_col {
        ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::core::{FieldDefinition, FieldTab, FieldType};
    use crate::db::DbConnection;
    use crate::db::migrate::collection::{create_collection_table, test_helpers::*};
    use crate::db::migrate::helpers::introspection::{get_table_columns, table_exists};
    use crate::db::migrate::helpers::join_tables::sync_join_tables;

    #[test]
    fn blocks_field_creates_join_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![FieldDefinition::builder("content", FieldType::Blocks).build()],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_content").unwrap());
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("_block_type"));
        assert!(cols.contains("data"));
    }

    #[test]
    fn blocks_inside_tabs_creates_join_table() {
        // Regression: blocks inside Tabs didn't get their join table created
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
        let tabs_field = FieldDefinition::builder("page_settings", FieldType::Tabs)
            .tabs(vec![FieldTab::new("Content", vec![blocks_field])])
            .build();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
                tabs_field,
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_content").unwrap(),
            "blocks table inside Tabs must be created"
        );
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_block_type"));
        assert!(cols.contains("data"));
    }

    #[test]
    fn blocks_inside_collapsible_creates_join_table() {
        // Regression: blocks inside Collapsible didn't get its join table created
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
        let collapsible_field = FieldDefinition::builder("advanced", FieldType::Collapsible)
            .fields(vec![blocks_field])
            .build();
        let def = simple_collection("posts", vec![text_field("title"), collapsible_field]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_content").unwrap(),
            "blocks table inside Collapsible must be created"
        );
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_block_type"));
        assert!(cols.contains("data"));
    }

    #[test]
    fn localized_blocks_creates_table_with_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("content", FieldType::Blocks)
                    .localized(true)
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_content").unwrap());
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_locale"));
    }

    #[test]
    fn existing_blocks_adds_locale_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute(
        "CREATE TABLE posts_content (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, _block_type TEXT, data TEXT)",
        &[],
    ).unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("content", FieldType::Blocks)
                    .localized(true)
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_locale"));
    }

    #[test]
    fn group_blocks_creates_prefixed_join_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("config", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("content", FieldType::Blocks).build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_config__content").unwrap(),
            "Group > Blocks should create prefixed join table posts_config__content"
        );
        let cols = get_table_columns(&conn, "posts_config__content").unwrap();
        assert!(
            cols.contains("_block_type"),
            "should have _block_type column"
        );
        assert!(cols.contains("data"), "should have data column");
    }

    #[test]
    fn group_group_blocks_creates_deeply_prefixed_join_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("outer", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("inner", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("content", FieldType::Blocks).build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_outer__inner__content").unwrap(),
            "Group > Group > Blocks should create double-prefixed join table"
        );
        let cols = get_table_columns(&conn, "posts_outer__inner__content").unwrap();
        assert!(cols.contains("_block_type"));
        assert!(cols.contains("data"));
    }

    #[test]
    fn localized_group_blocks_inherits_locale_column() {
        // Regression: blocks inside localized Groups missed _locale column
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .localized(true)
                    .fields(vec![
                        FieldDefinition::builder("content", FieldType::Blocks).build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_meta__content").unwrap();
        assert!(
            cols.contains("_locale"),
            "Blocks inside localized Group should inherit _locale column"
        );
    }
}
