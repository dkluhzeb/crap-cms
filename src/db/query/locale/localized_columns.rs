//! Column-level locale decisions: whether a stored column is localized, and
//! the per-locale columns it expands to.

use std::collections::HashSet;

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::FieldDefinition,
    db::query::{
        helpers::{locale_column, prefixed_name, walk_leaf_fields},
        localized_join_keys,
    },
};

/// Whether the stored column `name` — a leaf's flat column (`group__field`) or
/// one of its companions (`_tz`, `_lang`) — is localized, by its own flag or a parent
/// group's. `None` when no leaf stores that column. The one answer every
/// column-level locale decision uses: filters, sorts, full-text search and
/// reference scans.
pub(crate) fn column_is_localized(name: &str, fields: &[FieldDefinition]) -> Option<bool> {
    let mut result = None;

    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, inherited| {
        let column = prefixed_name(prefix, &field.name);
        let stores = field
            .columns_with_companions(&column)
            .any(|stored| stored == name);

        if result.is_none() && stores {
            result = Some(inherited || field.localized);
        }

        Ok(())
    });

    result
}

/// The stored columns of the leaf column `name`: one per configured locale when
/// it is localized and localization is on, else the column itself.
///
/// # Errors
///
/// Returns an error if a configured locale code has no column form.
pub(crate) fn stored_columns(
    name: &str,
    localized: bool,
    config: &LocaleConfig,
) -> Result<Vec<String>> {
    if !localized || !config.is_enabled() {
        return Ok(vec![name.to_string()]);
    }

    config
        .locales
        .iter()
        .map(|locale| locale_column(name, locale))
        .collect()
}

/// The flat keys a snapshot records per locale: localized columns — with their
/// companions — and localized join fields.
fn per_locale_bases(fields: &[FieldDefinition]) -> HashSet<String> {
    let mut bases: HashSet<String> = localized_join_keys(fields).into_iter().collect();

    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, inherited| {
        if (field.localized || inherited) && field.has_parent_column() {
            bases.extend(field.columns_with_companions(&prefixed_name(prefix, &field.name)));
        }

        Ok(())
    });

    bases
}

/// Every per-locale key of `fields` under `config` — `title__en`,
/// `seo__title__de`, `starts_tz__en`, a localized join field's
/// `gallery__slides__de` — the one list the snapshot build and the draft save
/// use to tell per-locale keys from a group's plain columns.
///
/// # Errors
///
/// Returns an error if a configured locale code has no column form.
pub(crate) fn per_locale_columns(
    fields: &[FieldDefinition],
    config: &LocaleConfig,
) -> Result<HashSet<String>> {
    let mut columns = HashSet::new();

    for base in per_locale_bases(fields) {
        for locale in &config.locales {
            columns.insert(locale_column(&base, locale)?);
        }
    }

    Ok(columns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::{
        locale::test_support::localized_code_lang_field, test_helpers::make_locale_config,
    };

    #[test]
    fn column_is_localized_knows_a_code_language_companion() {
        let fields = vec![localized_code_lang_field("snippet")];

        assert_eq!(column_is_localized("snippet_lang", &fields), Some(true));
    }

    /// Regression: the snapshot's per-locale key list omitted the `_lang`
    /// companion, so a version never recorded each locale's language pick.
    #[test]
    fn per_locale_columns_include_a_code_language_companion() {
        let fields = vec![localized_code_lang_field("snippet")];

        let columns = per_locale_columns(&fields, &make_locale_config()).unwrap();

        assert!(columns.contains("snippet_lang__en"), "got: {columns:?}");
        assert!(columns.contains("snippet_lang__de"), "got: {columns:?}");
    }
}
