//! The configured defaults a global's single row is created with.
//!
//! A global's row is inserted once, carrying nothing but its id, so without
//! this pass its values would come from the column `DEFAULT` — a clause that
//! can only be written when the column is created, and that therefore freezes
//! whatever the table was born with. The DDL keeps emitting it for
//! hand-written SQL; what the stored row holds is this module's doing.

use anyhow::{Context as _, Result};

use crate::{
    config::LocaleConfig,
    core::collection::GlobalDefinition,
    db::{
        DbConnection, DbValue,
        migrate::helpers::collect_column_specs,
        query::helpers::{column_value, locale_column, quote_ident},
    },
};

/// The column/value pairs a global's configured defaults write onto its row.
/// Each value goes through the shared encoder, so it is stored exactly as a
/// write of the same value would store it; a localized field fills every
/// locale's column.
fn default_row_values(
    def: &GlobalDefinition,
    locale_config: &LocaleConfig,
) -> Result<Vec<(String, DbValue)>> {
    let mut values = Vec::new();

    for spec in &collect_column_specs(&def.fields, locale_config) {
        let Some(default) = spec.field.default_value.as_ref() else {
            continue;
        };

        if spec.companion_text {
            continue;
        }

        let encoded = column_value(spec.field, default, None);

        if spec.is_localized {
            for locale in &locale_config.locales {
                values.push((locale_column(&spec.col_name, locale)?, encoded.clone()));
            }
        } else {
            values.push((spec.col_name.clone(), encoded));
        }
    }

    Ok(values)
}

/// Write the configured defaults onto the freshly created `default` row.
///
/// # Errors
///
/// Returns a backend error if a locale column name can't be built or the
/// UPDATE fails.
pub(super) fn apply_default_row_values(
    conn: &dyn DbConnection,
    table_name: &str,
    def: &GlobalDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let values = default_row_values(def, locale_config)?;

    if values.is_empty() {
        return Ok(());
    }

    let mut set_clauses = Vec::with_capacity(values.len());
    let mut params = Vec::with_capacity(values.len());

    for (idx, (col, value)) in values.into_iter().enumerate() {
        set_clauses.push(format!(
            "{} = {}",
            quote_ident(&col),
            conn.placeholder(idx + 1)
        ));
        params.push(value);
    }

    let sql = format!(
        "UPDATE {} SET {} WHERE id = 'default'",
        quote_ident(table_name),
        set_clauses.join(", ")
    );

    conn.execute(&sql, &params)
        .with_context(|| format!("Failed to apply default values to {table_name}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::{FieldDefinition, FieldType};
    use crate::db::migrate::collection::test_helpers::*;
    use crate::db::migrate::global::sync::sync_global_table;

    fn simple_global(slug: &str, fields: Vec<FieldDefinition>) -> GlobalDefinition {
        let mut def = GlobalDefinition::new(slug);
        def.fields = fields;
        def
    }

    /// The `default` row a global is created with holds the configured
    /// defaults, written by the migration rather than left to the column
    /// `DEFAULT` clause.
    #[test]
    fn the_default_row_holds_the_configured_defaults() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("site_name", FieldType::Text)
                    .default_value(json!("My Site"))
                    .build(),
                FieldDefinition::builder("scores", FieldType::Number)
                    .has_many(true)
                    .default_value(json!([1, 2]))
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        let row = conn
            .query_one(
                "SELECT site_name, scores FROM _global_settings WHERE id = 'default'",
                &[],
            )
            .unwrap()
            .unwrap();

        assert_eq!(
            row.get_opt_string("site_name").unwrap(),
            Some("My Site".to_string())
        );
        assert_eq!(
            row.get_opt_string("scores").unwrap(),
            Some("[1,2]".to_string())
        );
    }

    /// Regression: a global's has-many default became the column DEFAULT
    /// through the single-value coercion, so a number list stored nothing where
    /// a write of the same default stores the list. The same encoder decides
    /// both, so the row and the DDL agree.
    #[test]
    fn a_has_many_default_is_stored_as_its_write_stores_it() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("scores", FieldType::Number)
                    .has_many(true)
                    .default_value(json!([1, 2]))
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        conn.execute_batch("INSERT INTO _global_settings (id) VALUES ('with_default')")
            .unwrap();
        let row = conn
            .query_one(
                "SELECT scores FROM _global_settings WHERE id = 'with_default'",
                &[],
            )
            .unwrap()
            .unwrap();

        assert_eq!(
            row.opt_text_at(0).map_or(DbValue::Null, DbValue::Text),
            column_value(&def.fields[0], &json!([1, 2]), None)
        );
    }

    /// A localized field's default fills every locale's column, the way the
    /// column `DEFAULT` used to.
    #[test]
    fn a_localized_default_fills_every_locale_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("tagline", FieldType::Text)
                    .localized(true)
                    .default_value(json!("Hello"))
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &locale_en_de()).unwrap();

        let row = conn
            .query_one(
                "SELECT tagline__en, tagline__de FROM _global_settings WHERE id = 'default'",
                &[],
            )
            .unwrap()
            .unwrap();

        assert_eq!(row.get_string("tagline__en").unwrap(), "Hello");
        assert_eq!(row.get_string("tagline__de").unwrap(), "Hello");
    }
}
