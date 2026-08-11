//! Portability guard: reject any generated SQL identifier longer than
//! Postgres's 63-byte limit.
//!
//! Postgres silently TRUNCATES identifiers at `NAMEDATALEN - 1` (63 bytes), so
//! two long `{collection}_{group}__{field}__{locale}` combinations can collapse
//! to the *same* physical column or table name — undetectable, unrecoverable
//! cross-field / cross-collection data mixing. `SQLite` has no such limit, which
//! means the corruption only appears after a deploy to Postgres. This check
//! runs on **every** backend during migration so the problem surfaces in
//! development, and it errors rather than silently truncating.

use anyhow::{Result, bail};

use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, FieldType},
    db::query::helpers::{join_table, locale_column, prefixed_name, walk_leaf_fields},
};

use super::helpers::collect_column_specs;

/// Postgres truncates identifiers at `NAMEDATALEN - 1` = 63 bytes.
const MAX_IDENTIFIER_BYTES: usize = 63;

fn check_ident(kind: &str, name: &str, owner: &str) -> Result<()> {
    if name.len() > MAX_IDENTIFIER_BYTES {
        bail!(
            "{kind} '{name}' is {} bytes, over the {MAX_IDENTIFIER_BYTES}-byte SQL identifier \
             limit — Postgres would silently truncate it (risking a collision with another \
             identifier). Shorten the {owner}.",
            name.len()
        );
    }
    Ok(())
}

/// Validate every identifier a collection/global will generate on its physical
/// `table_base` (the collection slug, or `_global_{slug}` for a global).
pub(super) fn check_identifiers(
    table_base: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    check_ident("table name", table_base, "collection/global name")?;

    // Main-table columns, including group-prefixed, per-locale, and `_tz`/
    // `_lang` companion columns (all enumerated by `collect_column_specs`).
    for spec in collect_column_specs(fields, locale_config) {
        if spec.is_localized {
            for locale in &locale_config.locales {
                let col = locale_column(&spec.col_name, locale)?;
                check_ident("column", &col, "field / group / locale name")?;
            }
        } else {
            check_ident("column", &spec.col_name, "field / group name")?;
        }
    }

    // Join tables (array / blocks / relationship / upload), prefixed by any
    // enclosing group path — `{table_base}_{group}__{field}`.
    walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        if matches!(
            field.field_type,
            FieldType::Relationship | FieldType::Upload | FieldType::Array | FieldType::Blocks
        ) {
            let full = prefixed_name(prefix, &field.name);
            let table = join_table(table_base, &full);
            check_ident("join table", &table, "collection + group + field name")?;
        }
        Ok(())
    })?;

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::core::{FieldDefinition, FieldType};

    fn cfg() -> LocaleConfig {
        LocaleConfig::default()
    }

    #[test]
    fn short_identifiers_pass() {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("tags", FieldType::Array)
                .fields(vec![FieldDefinition::builder("v", FieldType::Text).build()])
                .build(),
        ];
        assert!(check_identifiers("posts", &fields, &cfg()).is_ok());
    }

    #[test]
    fn overlong_column_name_rejected() {
        let long = "a".repeat(70);
        let fields = vec![FieldDefinition::builder(&long, FieldType::Text).build()];
        let err = check_identifiers("posts", &fields, &cfg())
            .unwrap_err()
            .to_string();
        assert!(err.contains("over the 63-byte"), "got: {err}");
    }

    #[test]
    fn overlong_join_table_name_rejected() {
        // A 60-char collection + `_` + a 10-char field = 71 bytes for the join
        // table, over the limit, even though each name alone is fine.
        let collection = "c".repeat(60);
        let fields = vec![
            FieldDefinition::builder("tags_field", FieldType::Array)
                .fields(vec![FieldDefinition::builder("v", FieldType::Text).build()])
                .build(),
        ];
        let err = check_identifiers(&collection, &fields, &cfg())
            .unwrap_err()
            .to_string();
        assert!(err.contains("join table"), "got: {err}");
    }
}
