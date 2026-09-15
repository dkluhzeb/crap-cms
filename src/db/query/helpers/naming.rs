//! Generated column and table names: locale-suffixed columns, the `_tz` /
//! `_lang` companion columns, and join / global / version table names.

use anyhow::Result;

use crate::db::query::sanitize_locale;

// The companion suffixes belong to the field definition that decides a field
// has a companion; re-exported here so the column builders below and every
// `query::helpers::{LANG_SUFFIX, TZ_SUFFIX}` call site keep one name for them.
pub(crate) use crate::core::{LANG_SUFFIX, TZ_SUFFIX};

/// Build a locale-suffixed column name: `"field__en"`, `"seo__title__de"`.
///
/// Sanitizes the locale string before appending.
pub(crate) fn locale_column(field_name: &str, locale: &str) -> Result<String> {
    Ok(format!("{}__{}", field_name, sanitize_locale(locale)?))
}

/// Build a timezone companion column name: `"field_tz"`, `"seo__start_tz"`.
pub(crate) fn tz_column(name: &str) -> String {
    format!("{name}{TZ_SUFFIX}")
}

/// Whether `column` is stored by the field named `field`: its own column, a
/// column below it (`{field}__…` — per locale, or a group's sub-columns), or a
/// companion — a timezone date's `{field}_tz`, a code field's `{field}_lang` —
/// per locale too. Field names cannot contain `__` or end in a companion
/// suffix, so no sibling field matches.
pub(crate) fn column_belongs_to(column: &str, field: &str) -> bool {
    let Some(rest) = column.strip_prefix(field) else {
        return false;
    };

    let own = |rest: &str| rest.is_empty() || rest.starts_with("__");

    own(rest)
        || [TZ_SUFFIX, LANG_SUFFIX]
            .iter()
            .any(|suffix| rest.strip_prefix(suffix).is_some_and(own))
}

/// Build a code-language companion column name: `"snippet_lang"`,
/// `"meta__example_lang"`. Used by code fields with a non-empty
/// `admin.languages` allow-list — see `apply_code` in the field-context
/// builder.
pub(crate) fn lang_column(name: &str) -> String {
    format!("{name}{LANG_SUFFIX}")
}

/// Build a join table name: `"collection_field"`, `"posts_tags"`.
pub(crate) fn join_table(collection: &str, field: &str) -> String {
    format!("{collection}_{field}")
}

/// Build the table name for a global: `"_global_{slug}"`.
pub(crate) fn global_table(slug: &str) -> String {
    format!("_global_{slug}")
}

/// Build the version table name for a collection: `"_versions_{slug}"`.
/// Unquoted, like [`join_table`] / [`global_table`] — callers quote at the
/// interpolation site (e.g. via [`quote_ident`]).
///
/// [`quote_ident`]: crate::db::query::helpers::quote_ident
pub(crate) fn versions_table(slug: &str) -> String {
    format!("_versions_{slug}")
}
