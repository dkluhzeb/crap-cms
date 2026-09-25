//! The shape of a collection's search index: the columns it holds, the locale
//! each column's text belongs to, and how a search in one locale is confined
//! to that locale's text.
//!
//! `SQLite` keeps one FTS5 column per indexed column (`title__en`,
//! `title__de`, `body`), so a locale search is a column filter. Postgres keeps
//! one tsvector over every column (`tsv`) and, when any indexed field is
//! localized, one per locale (`tsv__en`, `tsv__de`) over that locale's columns
//! and the non-localized ones.

use anyhow::{Result, bail};

use crate::{
    config::LocaleConfig,
    core::CollectionDefinition,
    db::query::{
        LocaleContext, LocaleMode, column_is_localized, column_read_expr,
        fts::fields::get_fts_fields,
        helpers::{locale_column, quote_ident},
        is_valid_identifier,
    },
};

/// The Postgres full-text-search configuration of every `to_tsvector` and
/// `to_tsquery` call — one source, so the index and the query parse text the
/// same way.
pub(super) const PG_FTS_CONFIG: &str = "simple";

/// The Postgres tsvector column over every indexed column, in every locale.
pub(super) const PG_ALL_LOCALES_VECTOR: &str = "tsv";

/// One column of a search index.
pub(super) struct FtsColumn {
    /// The column's name, in the FTS table and on the collection row.
    pub name: String,
    /// The locale whose text the column holds; `None` for a field that is not
    /// localized, whose text belongs to every locale.
    pub locale: Option<String>,
    /// The SQL expression reading the column's text from the collection row —
    /// the same expression a read in that locale selects, so a value shown
    /// through the locale fallback is the value the index holds.
    pub read_expr: String,
}

/// One Postgres tsvector column and the index columns whose text it holds.
pub(super) struct PgVector {
    pub name: String,
    /// Positions in the index's column list.
    pub members: Vec<usize>,
}

impl PgVector {
    fn new(name: String, members: Vec<usize>) -> Self {
        Self { name, members }
    }
}

/// Where a search looks.
pub(super) enum SearchScope {
    /// Every indexed column, in every locale.
    Whole,
    /// One locale's text: its localized columns and the non-localized ones —
    /// by name on `SQLite`, as one tsvector column on Postgres.
    Locale {
        columns: Vec<String>,
        pg_vector: String,
    },
}

/// The columns of `def`'s search index under `locale_config`, in index order:
/// each indexed field once, or once per locale when it is localized.
///
/// # Errors
///
/// Returns an error if a field name is not a plain identifier or a locale code
/// has no column form.
pub(super) fn fts_columns(
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<Vec<FtsColumn>> {
    let mut columns = Vec::new();

    for field in get_fts_fields(def) {
        if !is_valid_identifier(&field) {
            bail!("Invalid FTS field name '{field}': must be alphanumeric/underscore");
        }

        push_field_columns(&mut columns, &field, def, locale_config)?;
    }

    Ok(columns)
}

/// Append the index column(s) of the indexed field `field`.
fn push_field_columns(
    columns: &mut Vec<FtsColumn>,
    field: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let localized =
        locale_config.is_enabled() && column_is_localized(field, &def.fields).unwrap_or(false);

    if !localized {
        columns.push(FtsColumn {
            name: field.to_string(),
            locale: None,
            read_expr: quote_ident(field),
        });

        return Ok(());
    }

    for locale in &locale_config.locales {
        // The configured fallback is kept: the index holds the text a read
        // in `locale` shows, fallback value included.
        let Some(read) = LocaleContext::from_locale_string(Some(locale.as_str()), locale_config)?
        else {
            bail!("Localized FTS column '{field}' with localization disabled");
        };

        columns.push(FtsColumn {
            name: locale_column(field, locale)?,
            locale: Some(locale.clone()),
            read_expr: column_read_expr(field, &def.fields, Some(&read))?,
        });
    }

    Ok(())
}

/// Whether any index column holds one locale's text.
fn has_localized(columns: &[FtsColumn]) -> bool {
    columns.iter().any(|c| c.locale.is_some())
}

/// Whether `column` holds text a search in `locale` matches.
fn in_scope(column: &FtsColumn, locale: &str) -> bool {
    column.locale.as_deref().is_none_or(|l| l == locale)
}

/// The Postgres tsvector columns of an index over `columns`: the all-locales
/// one, then one per configured locale when any column is localized.
///
/// # Errors
///
/// Returns an error if a locale code has no column form.
pub(super) fn pg_vectors(
    columns: &[FtsColumn],
    locale_config: &LocaleConfig,
) -> Result<Vec<PgVector>> {
    let mut vectors = vec![PgVector::new(
        PG_ALL_LOCALES_VECTOR.to_string(),
        (0..columns.len()).collect(),
    )];

    if !has_localized(columns) {
        return Ok(vectors);
    }

    for locale in &locale_config.locales {
        let members = columns
            .iter()
            .enumerate()
            .filter(|(_, c)| in_scope(c, locale))
            .map(|(i, _)| i)
            .collect();

        vectors.push(PgVector::new(
            locale_column(PG_ALL_LOCALES_VECTOR, locale)?,
            members,
        ));
    }

    Ok(vectors)
}

/// Where a search on `def` under `locale_ctx` looks: the requested locale's
/// text (the default locale's when none is requested), or every locale's for
/// an all-locales read, without localization, or when no indexed field is
/// localized.
///
/// # Errors
///
/// Returns an error if a field name or locale code has no column form.
pub(super) fn search_scope(
    def: &CollectionDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Result<SearchScope> {
    let Some(ctx) = locale_ctx else {
        return Ok(SearchScope::Whole);
    };

    if matches!(ctx.mode, LocaleMode::All) || !ctx.config.is_enabled() {
        return Ok(SearchScope::Whole);
    }

    let columns = fts_columns(def, &ctx.config)?;

    if !has_localized(&columns) {
        return Ok(SearchScope::Whole);
    }

    let locale = ctx.access_locale();

    Ok(SearchScope::Locale {
        columns: columns
            .into_iter()
            .filter(|c| in_scope(c, locale))
            .map(|c| c.name)
            .collect(),
        pg_vector: locale_column(PG_ALL_LOCALES_VECTOR, locale)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::FieldDefinition,
        db::migrate::collection::test_helpers::{locale_en_de, localized_field, text_field},
    };

    fn def_with(fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = fields;
        def
    }

    fn ctx(mode: LocaleMode, fallback: bool) -> LocaleContext {
        LocaleContext {
            mode,
            config: LocaleConfig {
                fallback,
                ..locale_en_de()
            },
        }
    }

    /// A localized column in a non-default locale reads through the fallback
    /// exactly as a read in that locale does; the default locale's and a
    /// non-localized column read the bare column.
    #[test]
    fn localized_columns_read_through_the_locale_fallback() {
        let def = def_with(vec![localized_field("title"), text_field("body")]);
        let config = ctx(LocaleMode::Default, true).config;

        let columns = fts_columns(&def, &config).unwrap();
        let read: Vec<(&str, Option<&str>, &str)> = columns
            .iter()
            .map(|c| (c.name.as_str(), c.locale.as_deref(), c.read_expr.as_str()))
            .collect();

        assert_eq!(
            read,
            vec![
                ("title__en", Some("en"), "\"title__en\""),
                (
                    "title__de",
                    Some("de"),
                    "COALESCE(\"title__de\", \"title__en\")"
                ),
                ("body", None, "\"body\""),
            ]
        );
    }

    #[test]
    fn pg_vectors_add_one_per_locale_only_when_something_is_localized() {
        let plain = def_with(vec![text_field("title"), text_field("body")]);
        let config = locale_en_de();

        let columns = fts_columns(&plain, &config).unwrap();
        let names: Vec<String> = pg_vectors(&columns, &config)
            .unwrap()
            .into_iter()
            .map(|v| v.name)
            .collect();
        assert_eq!(names, vec!["tsv"]);

        let localized = def_with(vec![localized_field("title"), text_field("body")]);
        let columns = fts_columns(&localized, &config).unwrap();
        let vectors = pg_vectors(&columns, &config).unwrap();
        let shape: Vec<(&str, &[usize])> = vectors
            .iter()
            .map(|v| (v.name.as_str(), v.members.as_slice()))
            .collect();

        assert_eq!(
            shape,
            vec![
                ("tsv", &[0, 1, 2][..]),
                ("tsv__en", &[0, 2][..]),
                ("tsv__de", &[1, 2][..]),
            ]
        );
    }

    #[test]
    fn a_single_locale_search_is_scoped_to_that_locale() {
        let def = def_with(vec![localized_field("title"), text_field("body")]);

        let SearchScope::Locale { columns, pg_vector } =
            search_scope(&def, Some(&ctx(LocaleMode::Single("de".into()), false))).unwrap()
        else {
            panic!("expected a locale scope");
        };

        assert_eq!(columns, vec!["title__de", "body"]);
        assert_eq!(pg_vector, "tsv__de");
    }

    #[test]
    fn a_default_locale_search_is_scoped_to_the_default_locale() {
        let def = def_with(vec![localized_field("title")]);

        let SearchScope::Locale { columns, .. } =
            search_scope(&def, Some(&ctx(LocaleMode::Default, false))).unwrap()
        else {
            panic!("expected a locale scope");
        };

        assert_eq!(columns, vec!["title__en"]);
    }

    #[test]
    fn all_locales_no_locale_and_unlocalized_indexes_search_everything() {
        let localized = def_with(vec![localized_field("title")]);
        let plain = def_with(vec![text_field("title")]);

        assert!(matches!(
            search_scope(&localized, Some(&ctx(LocaleMode::All, false))).unwrap(),
            SearchScope::Whole
        ));
        assert!(matches!(
            search_scope(&localized, None).unwrap(),
            SearchScope::Whole
        ));
        assert!(matches!(
            search_scope(&plain, Some(&ctx(LocaleMode::Single("de".into()), false))).unwrap(),
            SearchScope::Whole
        ));
    }
}
