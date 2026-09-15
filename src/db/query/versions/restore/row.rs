//! The row a restore writes back to, and the pass restoring its locale columns
//! and join rows from a snapshot.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    config::LocaleConfig,
    core::FieldDefinition,
    db::{
        DbConnection,
        query::versions::restore::{
            join_rows::restore_join_rows, locale_columns::restore_locale_values,
        },
    },
};

/// The row a restore writes back to.
pub(super) struct RestoreRow<'a> {
    pub(super) conn: &'a dyn DbConnection,
    pub(super) table: &'a str,
    pub(super) parent_id: &'a str,
    pub(super) fields: &'a [FieldDefinition],
}

/// Restore locale columns and join table data from a snapshot.
/// Group fields are always expanded to `field__subfield` sub-columns.
pub(super) fn restore_locale_and_join_data(
    conn: &dyn DbConnection,
    table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    obj: &Map<String, Value>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let row = RestoreRow {
        conn,
        table,
        parent_id,
        fields,
    };

    if locale_config.is_enabled() {
        restore_locale_values(&row, obj, locale_config)?;
    }

    restore_join_rows(&row, obj, locale_config)
}
