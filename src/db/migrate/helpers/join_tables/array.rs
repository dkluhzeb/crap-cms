//! Array-field join tables: create + alter.

use anyhow::{Context as _, Result};
use tracing::info;

use crate::config::LocaleConfig;
use crate::core::{FieldDefinition, FieldType, field::flatten_array_sub_fields};
use crate::db::DbConnection;
use crate::db::migrate::helpers::add_column_if_missing;
use crate::db::migrate::helpers::column_specs::ensure_locale_column;
use crate::db::migrate::helpers::introspection::{
    get_table_columns, sanitize_locale, table_exists,
};
use crate::db::query::helpers::{join_table, quote_ident, tz_column};

/// Sync an array join table (create or alter).
pub(super) fn sync_array_table(
    conn: &dyn DbConnection,
    collection_slug: &str,
    field: &FieldDefinition,
    full_name: &str,
    has_locale_col: bool,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table_name = join_table(collection_slug, full_name);
    let flat_subs = flatten_array_sub_fields(&field.fields);

    if table_exists(conn, &table_name)? {
        if has_locale_col {
            ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
        }

        alter_array_table(conn, &table_name, &flat_subs)?;
    } else {
        create_array_table(
            conn,
            &table_name,
            collection_slug,
            &flat_subs,
            has_locale_col,
            locale_config,
        )?;
    }

    Ok(())
}

/// Create a new array join table.
fn create_array_table(
    conn: &dyn DbConnection,
    table_name: &str,
    collection_slug: &str,
    flat_subs: &[&FieldDefinition],
    has_locale_col: bool,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut columns = vec![
        "id TEXT PRIMARY KEY".to_string(),
        format!(
            "parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE",
            quote_ident(collection_slug)
        ),
        "_order INTEGER NOT NULL DEFAULT 0".to_string(),
    ];

    if has_locale_col {
        columns.push(format!(
            "_locale TEXT NOT NULL DEFAULT '{}'",
            sanitize_locale(&locale_config.default_locale)?
        ));
    }

    for sub_field in flat_subs {
        columns.push(format!(
            "{} {}",
            quote_ident(&sub_field.name),
            conn.column_type_for(&sub_field.field_type)
        ));

        if sub_field.field_type == FieldType::Date && sub_field.timezone {
            columns.push(format!("{} TEXT", quote_ident(&tz_column(&sub_field.name))));
        }
    }

    let sql = format!("CREATE TABLE \"{}\" ({})", table_name, columns.join(", "));

    info!("Creating array table: {}", table_name);
    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to create array table {table_name}"))?;

    Ok(())
}

/// Add missing sub-field columns to an existing array table.
fn alter_array_table(
    conn: &dyn DbConnection,
    table_name: &str,
    flat_subs: &[&FieldDefinition],
) -> Result<()> {
    let existing = get_table_columns(conn, table_name)?;

    for sub_field in flat_subs {
        let col_def = format!(
            "{} {}",
            quote_ident(&sub_field.name),
            conn.column_type_for(&sub_field.field_type)
        );
        add_column_if_missing(conn, table_name, &sub_field.name, &col_def, &existing)?;

        if sub_field.field_type == FieldType::Date && sub_field.timezone {
            let tz_col = tz_column(&sub_field.name);
            let tz_def = format!("{} TEXT", quote_ident(&tz_col));
            add_column_if_missing(conn, table_name, &tz_col, &tz_def, &existing)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::FieldTab;
    use crate::db::migrate::collection::{create_collection_table, test_helpers::*};
    use crate::db::migrate::helpers::join_tables::sync_join_tables;

    #[test]
    fn array_field_creates_join_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .fields(vec![text_field("name")])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_items").unwrap());
        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("_order"));
        assert!(cols.contains("name"));
    }

    #[test]
    fn array_inside_row_creates_join_table() {
        // Regression: array inside Row didn't get its join table created
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let array_field = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text_field("label"), text_field("value")])
            .build();
        let row_field = FieldDefinition::builder("main_row", FieldType::Row)
            .fields(vec![array_field])
            .build();
        let def = simple_collection("posts", vec![text_field("title"), row_field]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_items").unwrap(),
            "array table inside Row must be created"
        );
        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("label"));
        assert!(cols.contains("value"));
    }

    #[test]
    fn localized_array_creates_table_with_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .localized(true)
                    .fields(vec![text_field("label")])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_items").unwrap());
        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("_locale"));
    }

    #[test]
    fn existing_array_adds_new_subfield_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute("CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT)", &[]).unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .fields(vec![text_field("label"), text_field("value")])
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(
            cols.contains("value"),
            "New sub-field column should be added"
        );
    }

    #[test]
    fn existing_array_adds_locale_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute("CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT)", &[]).unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .localized(true)
                    .fields(vec![text_field("label")])
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("_locale"));
    }

    #[test]
    fn array_with_tabs_creates_flat_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let array_field = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                text_field("plain"),
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![
                        FieldTab::new("General", vec![text_field("title")]),
                        FieldTab::new("Content", vec![text_field("body")]),
                    ])
                    .build(),
            ])
            .build();
        let def = simple_collection("posts", vec![text_field("name"), array_field]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_items").unwrap());
        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("plain"), "plain sub-field column");
        assert!(cols.contains("title"), "title from tabs should be promoted");
        assert!(cols.contains("body"), "body from tabs should be promoted");
        assert!(
            !cols.contains("layout"),
            "layout wrapper should NOT be a column"
        );
    }

    #[test]
    fn group_array_creates_prefixed_join_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("config", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("items", FieldType::Array)
                            .fields(vec![
                                text_field("name"),
                                FieldDefinition::builder("score", FieldType::Number).build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_config__items").unwrap(),
            "Group > Array should create prefixed join table posts_config__items"
        );
        let cols = get_table_columns(&conn, "posts_config__items").unwrap();
        assert!(cols.contains("name"), "should have name column");
        assert!(cols.contains("score"), "should have score column");
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("_order"));
    }

    #[test]
    fn group_array_localized_creates_table_with_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("config", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("items", FieldType::Array)
                            .localized(true)
                            .fields(vec![text_field("label")])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(
            table_exists(&conn, "posts_config__items").unwrap(),
            "Group > localized Array should create prefixed join table"
        );
        let cols = get_table_columns(&conn, "posts_config__items").unwrap();
        assert!(
            cols.contains("_locale"),
            "localized Array inside Group should have _locale column"
        );
        assert!(cols.contains("label"));
    }

    #[test]
    fn group_group_array_creates_deeply_prefixed_join_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("outer", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("inner", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("items", FieldType::Array)
                                    .fields(vec![text_field("name")])
                                    .build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_outer__inner__items").unwrap(),
            "Group > Group > Array should create double-prefixed join table"
        );
        let cols = get_table_columns(&conn, "posts_outer__inner__items").unwrap();
        assert!(cols.contains("name"));
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("_order"));
    }

    #[test]
    fn array_with_row_creates_flat_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let array_field = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("row_wrap", FieldType::Row)
                    .fields(vec![text_field("x"), text_field("y")])
                    .build(),
            ])
            .build();
        let def = simple_collection("posts", vec![array_field]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("x"), "x from row should be promoted");
        assert!(cols.contains("y"), "y from row should be promoted");
        assert!(
            !cols.contains("row_wrap"),
            "row wrapper should NOT be a column"
        );
    }

    #[test]
    fn array_date_with_timezone_creates_tz_column() {
        // Regression: array sub-fields with timezone Date didn't get _tz companion column
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let array_field = FieldDefinition::builder("events", FieldType::Array)
            .fields(vec![
                text_field("title"),
                FieldDefinition::builder("scheduled_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ])
            .build();
        let def = simple_collection("posts", vec![array_field]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts_events").unwrap();
        assert!(cols.contains("scheduled_at"), "date column should exist");
        assert!(
            cols.contains("scheduled_at_tz"),
            "timezone companion column should exist for Date+timezone in array"
        );
    }

    #[test]
    fn existing_array_adds_tz_column_on_alter() {
        // Regression: ALTER path also missed _tz companion columns
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute(
        "CREATE TABLE posts_events (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, title TEXT)",
        &[],
    )
    .unwrap();

        let array_field = FieldDefinition::builder("events", FieldType::Array)
            .fields(vec![
                text_field("title"),
                FieldDefinition::builder("scheduled_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ])
            .build();
        let def = simple_collection("posts", vec![array_field]);
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts_events").unwrap();
        assert!(cols.contains("scheduled_at"), "date column should be added");
        assert!(
            cols.contains("scheduled_at_tz"),
            "timezone companion column should be added on alter"
        );
    }

    #[test]
    fn localized_group_array_inherits_locale_column() {
        // Regression: arrays inside localized Groups missed _locale column
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .localized(true)
                    .fields(vec![
                        FieldDefinition::builder("items", FieldType::Array)
                            .fields(vec![text_field("label")])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_meta__items").unwrap();
        assert!(
            cols.contains("_locale"),
            "Array inside localized Group should inherit _locale column"
        );
    }
}
