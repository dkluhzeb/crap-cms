//! Column-level locale decisions: whether a stored column is localized, and
//! the per-locale columns it expands to.

use std::collections::HashSet;

use anyhow::{Result, bail};

use crate::{
    config::LocaleConfig,
    core::FieldDefinition,
    db::{
        LocaleContext,
        query::{
            helpers::{locale_column, prefixed_name, qualified_ident, walk_leaf_fields},
            is_valid_identifier, localized_join_keys,
        },
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

/// The SQL expression every read of the parent-table column `column` goes
/// through: [`ReadLocale::column_expr`] when the column is localized and
/// localization is on, the quoted column itself otherwise.
///
/// The SELECT, the WHERE comparand, the ORDER BY key and the keyset cursor's
/// comparand all take their expression from here, so a fallback value the
/// listing shows is the same value a filter matches, a sort orders by, and a
/// page boundary compares against.
///
/// An all-locales read has no single column to compare — it takes the default
/// locale's, as the write column and the join-row locale do.
///
/// [`ReadLocale::column_expr`]: crate::db::query::ReadLocale::column_expr
///
/// # Errors
///
/// Returns an error if `column` is not a plain identifier, or if a configured
/// locale code has no column form.
pub(crate) fn column_read_expr(
    column: &str,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<String> {
    read_expr_in(None, column, fields, locale_ctx)
}

/// [`column_read_expr`] with every column qualified by `table` — the same read
/// expression, for a comparand evaluated inside a subquery whose own FROM item
/// (a `json_each` expansion, say) exposes columns that would shadow a bare name.
///
/// # Errors
///
/// Returns an error if `column` is not a plain identifier, or if a configured
/// locale code has no column form.
pub(crate) fn qualified_column_read_expr(
    table: &str,
    column: &str,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<String> {
    read_expr_in(Some(table), column, fields, locale_ctx)
}

fn read_expr_in(
    table: Option<&str>,
    column: &str,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<String> {
    if !is_valid_identifier(column) {
        bail!("Invalid field name '{column}': must be alphanumeric/underscore");
    }

    let localized = locale_ctx
        .filter(|ctx| ctx.config.is_enabled() && column_is_localized(column, fields) == Some(true));

    let Some(ctx) = localized else {
        return Ok(qualified_ident(table, column));
    };

    ctx.rows_read_locale().qualified_column_expr(table, column)
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
    use crate::{
        core::FieldType,
        db::{
            LocaleMode,
            query::{
                locale::test_support::localized_code_lang_field,
                test_helpers::{make_field, make_locale_config, make_localized_field},
            },
        },
    };

    fn ctx(mode: LocaleMode) -> LocaleContext {
        LocaleContext {
            mode,
            config: make_locale_config(),
        }
    }

    fn title_fields() -> Vec<FieldDefinition> {
        vec![
            make_localized_field("title", FieldType::Text),
            make_field("slug", FieldType::Text),
        ]
    }

    /// A localized column is read through the same fallback `COALESCE` the
    /// SELECT emits — a filter, sort or keyset comparing the bare locale column
    /// disagreed with the values the listing showed.
    #[test]
    fn a_localized_column_reads_through_the_fallback_expression() {
        let de = ctx(LocaleMode::Single("de".into()));

        let expr = column_read_expr("title", &title_fields(), Some(&de)).unwrap();

        assert_eq!(expr, "COALESCE(\"title__de\", \"title__en\")");
    }

    /// The qualified read expression is the same expression with every column
    /// prefixed by its table, localized or not.
    #[test]
    fn a_qualified_read_expression_prefixes_every_column() {
        let de = ctx(LocaleMode::Single("de".into()));
        let fields = title_fields();

        assert_eq!(
            qualified_column_read_expr("posts", "title", &fields, Some(&de)).unwrap(),
            "COALESCE(\"posts\".\"title__de\", \"posts\".\"title__en\")"
        );
        assert_eq!(
            qualified_column_read_expr("posts", "slug", &fields, Some(&de)).unwrap(),
            "\"posts\".\"slug\""
        );
    }

    /// The default locale has nothing to fall back to, and a shared column has
    /// no locale at all — but every form is quoted, so a field named after a
    /// SQL keyword compares against the column rather than against whatever
    /// Postgres makes of the bare word.
    #[test]
    fn the_default_locale_and_shared_columns_read_a_single_quoted_column() {
        let fields = title_fields();
        let default_ctx = ctx(LocaleMode::Default);

        assert_eq!(
            column_read_expr("title", &fields, Some(&default_ctx)).unwrap(),
            "\"title__en\""
        );
        assert_eq!(
            column_read_expr("slug", &fields, Some(&default_ctx)).unwrap(),
            "\"slug\""
        );
        assert_eq!(
            column_read_expr("title", &fields, None).unwrap(),
            "\"title\""
        );
    }

    /// An all-locales read has no single column to compare against, so it
    /// takes the default locale's — the same column its writes target.
    #[test]
    fn an_all_locales_read_compares_the_default_locale_column() {
        let all = ctx(LocaleMode::All);

        let expr = column_read_expr("title", &title_fields(), Some(&all)).unwrap();

        assert_eq!(expr, "\"title__en\"");
    }

    /// The expression is interpolated into SQL, so a column name that is not a
    /// plain identifier is refused rather than quoted and passed through.
    #[test]
    fn a_column_name_that_is_not_an_identifier_is_refused() {
        let err = column_read_expr("title; DROP TABLE posts", &title_fields(), None).unwrap_err();

        assert!(err.to_string().contains("Invalid field name"), "{err}");
    }

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
