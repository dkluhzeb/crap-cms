//! Recursive walker that dispatches each field to the per-type sync helper.

use anyhow::Result;

use crate::config::LocaleConfig;
use crate::core::{FieldDefinition, FieldType};
use crate::db::DbConnection;
use crate::db::query::helpers::prefixed_name;

use super::array::sync_array_table;
use super::blocks::sync_blocks_table;
use super::relationship::sync_relationship_table;

/// Sync join tables for has-many relationships and array fields.
pub(in crate::db::migrate) fn sync_join_tables(
    conn: &dyn DbConnection,
    collection_slug: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    sync_join_tables_inner(conn, collection_slug, fields, locale_config, "", false)
}

fn sync_join_tables_inner(
    conn: &dyn DbConnection,
    collection_slug: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
    prefix: &str,
    inherited_localized: bool,
) -> Result<()> {
    for field in fields {
        let has_locale_col = (inherited_localized || field.localized) && locale_config.is_enabled();
        let full_name = prefixed_name(prefix, &field.name);

        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                sync_relationship_table(
                    conn,
                    collection_slug,
                    field,
                    &full_name,
                    has_locale_col,
                    locale_config,
                )?;
            }
            FieldType::Array => {
                sync_array_table(
                    conn,
                    collection_slug,
                    field,
                    &full_name,
                    has_locale_col,
                    locale_config,
                )?;
            }
            FieldType::Blocks => {
                sync_blocks_table(
                    conn,
                    collection_slug,
                    &full_name,
                    has_locale_col,
                    locale_config,
                )?;
            }
            FieldType::Group => {
                sync_join_tables_inner(
                    conn,
                    collection_slug,
                    &field.fields,
                    locale_config,
                    &full_name,
                    inherited_localized || field.localized,
                )?;
            }
            FieldType::Row | FieldType::Collapsible => {
                sync_join_tables_inner(
                    conn,
                    collection_slug,
                    &field.fields,
                    locale_config,
                    prefix,
                    inherited_localized,
                )?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    sync_join_tables_inner(
                        conn,
                        collection_slug,
                        &tab.fields,
                        locale_config,
                        prefix,
                        inherited_localized,
                    )?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}
