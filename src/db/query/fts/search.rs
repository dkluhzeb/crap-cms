//! FTS search: turn a caller's search term into an index query, and build the
//! WHERE / ORDER BY fragments that run it.
//!
//! Supports `SQLite` (FTS5 MATCH) and `PostgreSQL` (tsvector @@ tsquery). Both
//! backends read a term the same way: whitespace separates words, a word
//! matches as a prefix of an indexed word, every word must match, and a word
//! with no letter or digit carries nothing to match and is dropped — so a term
//! of only punctuation is no search at all, like an empty or whitespace term.

use anyhow::Result;

use crate::{
    config::{QueryConfig, query_limits},
    core::{Builder, CollectionDefinition},
    db::{
        DbConnection, LocaleContext,
        query::{
            filter::invalid_query,
            fts::layout::{PG_ALL_LOCALES_VECTOR, PG_FTS_CONFIG, SearchScope, search_scope},
            helpers::quote_ident,
        },
    },
};

/// The key a rejected search term names.
const SEARCH_KEY: &str = "search";

/// FTS5 table name for a collection.
pub(super) fn fts_table_name(slug: &str) -> String {
    format!("_fts_{slug}")
}

/// Check if a table exists in the database.
pub(super) fn table_exists(conn: &dyn DbConnection, name: &str) -> bool {
    conn.table_exists(name).unwrap_or(false)
}

/// A caller's search: the collection, the term, and the locale whose text the
/// term matches (see [`search_scope`]).
#[derive(Builder)]
pub(crate) struct FtsSearch<'a> {
    #[builder(required)]
    pub slug: &'a str,
    #[builder(required)]
    pub def: &'a CollectionDefinition,
    #[builder(required)]
    pub term: &'a str,
    pub locale_ctx: Option<&'a LocaleContext>,
}

/// The words of a search term that carry something to match — each holds at
/// least one letter or digit.
pub(super) struct SearchWords(Vec<String>);

impl SearchWords {
    /// Split `term` into its searchable words, `None` when it has none.
    ///
    /// # Errors
    ///
    /// Returns a typed invalid-query error (naming `search`) when the term is
    /// longer than `max_search_length` characters or has more than
    /// `max_search_terms` words.
    pub(super) fn parse(term: &str, limits: &QueryConfig) -> Result<Option<Self>> {
        check_search_limits(term, limits)?;

        let words: Vec<String> = term
            .split_whitespace()
            .filter(|word| word.chars().any(char::is_alphanumeric))
            .map(str::to_string)
            .collect();

        if words.is_empty() {
            return Ok(None);
        }

        Ok(Some(Self(words)))
    }

    /// The whole-index query for `conn`'s backend — the index-membership probe
    /// the sync tests use.
    #[cfg(test)]
    pub(super) fn backend_query(&self, conn: &dyn DbConnection) -> String {
        if conn.is_postgres() {
            self.tsquery()
        } else {
            self.fts5_query()
        }
    }

    /// FTS5: every word a quoted phrase (embedded `"` doubled) with a `*`
    /// prefix marker, joined by the implicit AND. Quoting makes every
    /// FTS5-special character literal inside the phrase, so no term can break
    /// out into the MATCH grammar; the result is always bound, never
    /// interpolated.
    fn fts5_query(&self) -> String {
        self.0
            .iter()
            .map(|word| format!("\"{}\" *", word.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// tsquery: every word a quoted lexeme (`'` doubled, `\\` escaped) with the
    /// `:*` prefix marker, joined by `&`. A quoted lexeme is run through the
    /// same parser `to_tsvector` indexed the text with, so an email, a
    /// hyphenated word or a decimal matches the tokens it was indexed as, and
    /// no tsquery operator in the word is ever read as one.
    fn tsquery(&self) -> String {
        self.0
            .iter()
            .map(|word| format!("'{}':*", word.replace('\\', "\\\\").replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(" & ")
    }
}

/// Refuse a search term longer or wordier than `limits` allow — each word is
/// one more phrase to match against the index, per row for a ranked sort.
fn check_search_limits(term: &str, limits: &QueryConfig) -> Result<()> {
    let max_length = limits.max_search_length.get();

    if term.chars().count() > max_length {
        return Err(invalid_query(
            SEARCH_KEY,
            format!(
                "search term too long (at most {max_length} characters, \
                 `[query] max_search_length`)"
            ),
        ));
    }

    let max_terms = limits.max_search_terms.get();

    if term.split_whitespace().count() > max_terms {
        return Err(invalid_query(
            SEARCH_KEY,
            format!("too many search words (at most {max_terms}, `[query] max_search_terms`)"),
        ));
    }

    Ok(())
}

/// A search ready to run: the FTS table, where in it to look, and the query
/// text bound as a parameter.
struct PreparedSearch {
    table: String,
    scope: SearchScope,
    query: String,
}

impl PreparedSearch {
    /// The quoted Postgres tsvector column the search reads.
    fn pg_vector(&self) -> String {
        match &self.scope {
            SearchScope::Whole => quote_ident(PG_ALL_LOCALES_VECTOR),
            SearchScope::Locale { pg_vector, .. } => quote_ident(pg_vector),
        }
    }
}

/// Resolve `search` against `conn`: `None` when the term has no searchable
/// word or the collection has no index — the search then filters nothing.
fn prepare(conn: &dyn DbConnection, search: &FtsSearch<'_>) -> Result<Option<PreparedSearch>> {
    let Some(words) = SearchWords::parse(search.term, query_limits())? else {
        return Ok(None);
    };

    let table = fts_table_name(search.slug);

    if !table_exists(conn, &table) {
        return Ok(None);
    }

    let scope = search_scope(search.def, search.locale_ctx)?;

    let query = if conn.is_postgres() {
        words.tsquery()
    } else {
        fts5_match(&words, &scope)
    };

    Ok(Some(PreparedSearch {
        table,
        scope,
        query,
    }))
}

/// The FTS5 MATCH text for `words` in `scope`: a locale scope confines every
/// word to that locale's columns with a column filter.
fn fts5_match(words: &SearchWords, scope: &SearchScope) -> String {
    let terms = words.fts5_query();

    let SearchScope::Locale { columns, .. } = scope else {
        return terms;
    };

    let colset = columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(" ");

    format!("{{{colset}}} : ({terms})")
}

/// Build an `id IN (SELECT id FROM _fts_{slug} …)` clause matching `search`.
///
/// Returns `Ok(None)` if the FTS table doesn't exist or the term has no
/// searchable word; `Ok(Some((clause_fragment, query)))` otherwise, the query
/// to be bound at `param_index`.
///
/// # Errors
///
/// Returns a typed invalid-query error for a term over the `[query]` search
/// limits, or an error if a field name or locale code has no column form.
pub(crate) fn fts_where_clause(
    conn: &dyn DbConnection,
    search: &FtsSearch<'_>,
    param_index: usize,
) -> Result<Option<(String, String)>> {
    let Some(prepared) = prepare(conn, search)? else {
        return Ok(None);
    };

    let fts_table = &prepared.table;
    let placeholder = conn.placeholder(param_index);

    let clause = if conn.is_postgres() {
        format!(
            "id IN (SELECT id FROM {fts_table} WHERE {} @@ to_tsquery('{PG_FTS_CONFIG}', {placeholder}))",
            prepared.pg_vector()
        )
    } else {
        format!("id IN (SELECT id FROM {fts_table} WHERE {fts_table} MATCH {placeholder})")
    };

    Ok(Some((clause, prepared.query)))
}

/// ORDER BY clause sorting by search relevance, best first, with a stable
/// `id` tiebreaker. Correlated against the FTS table so it composes with the
/// normal find pipeline (access constraints, status axis, soft-delete,
/// pagination) untouched: `SQLite` ranks via `bm25()` (lower = better), and
/// `Postgres` via `ts_rank` (higher = better, NULLS LAST for safety). Ranks the
/// same locale's text the search filter matches.
///
/// Returns `Ok(None)` when the FTS table doesn't exist or the term has no
/// searchable word — callers fall back to a plain stable order, mirroring the
/// search *filter*.
///
/// # Errors
///
/// Returns a typed invalid-query error for a term over the `[query]` search
/// limits, or an error if a field name or locale code has no column form.
pub(crate) fn fts_rank_order_by(
    conn: &dyn DbConnection,
    search: &FtsSearch<'_>,
    param_index: usize,
) -> Result<Option<(String, String)>> {
    let Some(prepared) = prepare(conn, search)? else {
        return Ok(None);
    };

    let fts_table = &prepared.table;
    let slug = search.slug;
    let placeholder = conn.placeholder(param_index);

    let clause = if conn.is_postgres() {
        format!(
            " ORDER BY (SELECT ts_rank(f.{}, to_tsquery('{PG_FTS_CONFIG}', {placeholder})) FROM {fts_table} f WHERE f.id = \"{slug}\".id) DESC NULLS LAST, id ASC",
            prepared.pg_vector()
        )
    } else {
        format!(
            " ORDER BY (SELECT bm25({fts_table}) FROM {fts_table} WHERE {fts_table}.id = \"{slug}\".id AND {fts_table} MATCH {placeholder}) ASC, id ASC"
        )
    };

    Ok(Some((clause, prepared.query)))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::{CrapConfig, LocaleConfig},
        core::{FieldDefinition, FieldType, ValidationError},
        db::{
            BoxedConnection, DbValue, LocaleMode,
            migrate::collection::test_helpers::text_field,
            pool,
            query::fts::{FtsIndex, sync::sync_fts_table},
        },
    };

    fn simple_def(fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = fields;
        def
    }

    fn setup_db() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                body TEXT,
                status TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();
        (dir, conn)
    }

    fn insert_post(conn: &dyn DbConnection, id: &str, title: &str, body: &str) {
        conn.execute(
            "INSERT INTO posts (id, title, body, created_at, updated_at) VALUES (?1, ?2, ?3, datetime('now'), datetime('now'))",
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(title.to_string()),
                DbValue::Text(body.to_string()),
            ],
        ).unwrap();
    }

    fn words(term: &str) -> Option<SearchWords> {
        SearchWords::parse(term, &QueryConfig::default()).unwrap()
    }

    fn fts5(term: &str) -> String {
        words(term).map(|w| w.fts5_query()).unwrap_or_default()
    }

    fn tsquery(term: &str) -> String {
        words(term).map(|w| w.tsquery()).unwrap_or_default()
    }

    // ── SearchWords ─────────────────────────────────────────────────────

    #[test]
    fn fts5_query_quotes_each_word_as_a_prefix_phrase() {
        assert_eq!(fts5("hello world"), "\"hello\" * \"world\" *");
        assert_eq!(fts5("foo's bar"), "\"foo's\" * \"bar\" *");
        assert_eq!(fts5("hello"), "\"hello\" *");
        assert_eq!(
            fts5("say \"hello\" please"),
            "\"say\" * \"\"\"hello\"\"\" * \"please\" *"
        );
    }

    /// Regression: the Postgres query stripped every non-alphanumeric
    /// character, so `jane@example.com` became `janeexamplecom:*` and never
    /// matched the `jane@example.com` token `to_tsvector` indexed. Each word is
    /// now a quoted lexeme that the same parser splits the same way.
    #[test]
    fn tsquery_quotes_each_word_as_a_prefix_lexeme() {
        assert_eq!(tsquery("jane@example.com"), "'jane@example.com':*");
        assert_eq!(tsquery("well-known 3.14"), "'well-known':* & '3.14':*");
        assert_eq!(tsquery("O'Brien"), "'O''Brien':*");
        assert_eq!(tsquery("a\\b"), "'a\\\\b':*");
    }

    /// tsquery operators inside a word stay literal — the whole word is one
    /// quoted lexeme with its quotes doubled, so `&`, `|`, `!`, `:`,
    /// parentheses and a stray quote cannot restructure the query.
    #[test]
    fn tsquery_keeps_operators_inside_the_quoted_lexeme() {
        assert_eq!(tsquery("a&b|!c:(d)"), "'a&b|!c:(d)':*");
        assert_eq!(tsquery("x':*|'y"), "'x'':*|''y':*");
    }

    #[test]
    fn empty_and_whitespace_terms_have_no_words() {
        assert!(words("").is_none());
        assert!(words("   ").is_none());
    }

    /// Regression: a term of only punctuation matched nothing on `SQLite` but
    /// everything on Postgres. It carries nothing to match on either backend,
    /// so it is no search at all — like an empty term — and a punctuation word
    /// beside a real one is dropped on both.
    #[test]
    fn punctuation_only_words_are_dropped_on_both_backends() {
        assert!(words("---").is_none());
        assert!(words("@ \" -").is_none());
        assert_eq!(fts5("rust ---"), "\"rust\" *");
        assert_eq!(tsquery("rust ---"), "'rust':*");
    }

    fn limits(length: usize, terms: usize) -> QueryConfig {
        QueryConfig {
            max_search_length: NonZeroUsize::new(length).unwrap(),
            max_search_terms: NonZeroUsize::new(terms).unwrap(),
            ..QueryConfig::default()
        }
    }

    fn search_error(term: &str, limits: &QueryConfig) -> String {
        let err = SearchWords::parse(term, limits).err().expect("rejected");
        let ve = err.downcast_ref::<ValidationError>().expect("typed error");

        assert_eq!(ve.errors[0].field, "search");

        ve.errors[0].message.clone()
    }

    /// Regression: a search term had no size bound — a megabyte term became a
    /// hundred-thousand-phrase MATCH evaluated per row.
    #[test]
    fn a_term_over_the_search_limits_is_refused() {
        assert!(search_error("abcdef", &limits(5, 10)).contains("max_search_length"));
        assert!(search_error("a b c", &limits(100, 2)).contains("max_search_terms"));

        assert!(SearchWords::parse("abcde", &limits(5, 10)).is_ok());
        assert!(SearchWords::parse("a b", &limits(100, 2)).is_ok());
    }

    /// The length limit counts characters, not bytes.
    #[test]
    fn the_length_limit_counts_characters() {
        assert!(SearchWords::parse("äöü", &limits(3, 10)).is_ok());
    }

    // ── fts_where_clause ────────────────────────────────────────────────

    fn search<'a>(def: &'a CollectionDefinition, term: &'a str) -> FtsSearch<'a> {
        FtsSearch::builder("posts", def, term).build()
    }

    fn index(conn: &dyn DbConnection, def: &CollectionDefinition) {
        sync_fts_table(
            conn,
            &FtsIndex::builder("posts", def, &LocaleConfig::default()).build(),
        )
        .unwrap();
    }

    fn matching_ids(
        conn: &dyn DbConnection,
        def: &CollectionDefinition,
        term: &str,
    ) -> Vec<String> {
        let (clause, query) = fts_where_clause(conn, &search(def, term), 1)
            .unwrap()
            .expect("clause");
        let sql = format!("SELECT id FROM posts WHERE {clause} ORDER BY id");

        conn.query_all(&sql, &[DbValue::Text(query)])
            .unwrap()
            .into_iter()
            .filter_map(|row| row.opt_text_at(0))
            .collect()
    }

    #[test]
    fn where_clause_with_fts_table() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "1", "Hello", "");
        let def = simple_def(vec![text_field("title")]);
        index(&conn, &def);

        let (clause, query) = fts_where_clause(&conn, &search(&def, "Hello"), 1)
            .unwrap()
            .expect("clause");
        assert!(clause.contains("_fts_posts"));
        assert_eq!(query, "\"Hello\" *");
    }

    #[test]
    fn where_clause_no_fts_table() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);

        let clause = fts_where_clause(&conn, &search(&def, "Hello"), 1).unwrap();

        assert!(clause.is_none());
    }

    #[test]
    fn where_clause_empty_query() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);
        index(&conn, &def);

        for term in ["", "   ", "---"] {
            let clause = fts_where_clause(&conn, &search(&def, term), 1).unwrap();

            assert!(clause.is_none(), "term {term:?}");
        }
    }

    /// The size limits apply whether or not the collection has an index, so a
    /// term is refused the same way on every collection.
    #[test]
    fn where_clause_refuses_an_oversized_term_without_an_index() {
        let (_dir, conn) = setup_db();
        let def = simple_def(vec![text_field("title")]);
        let term = "w ".repeat(QueryConfig::default().max_search_terms.get() + 1);

        assert!(fts_where_clause(&conn, &search(&def, &term), 1).is_err());
    }

    #[test]
    fn fts_where_clause_integrates_with_query() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "1", "Rust Programming", "Learn Rust");
        insert_post(&conn, "2", "Python Programming", "Learn Python");
        insert_post(&conn, "3", "Rust Web", "Web development with Rust");

        let def = simple_def(vec![text_field("title"), text_field("body")]);
        index(&conn, &def);

        assert_eq!(matching_ids(&conn, &def, "Rust"), vec!["1", "3"]);
    }

    /// The cross-backend corpus (the Postgres twin lives in the pg harness):
    /// an email, a hyphenated word, a decimal and an apostrophe each find
    /// exactly their document.
    #[test]
    fn punctuated_words_match() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "1", "jane@example.com", "well-known");
        insert_post(&conn, "2", "pi is 3.14", "O'Brien");
        insert_post(&conn, "3", "unrelated", "nothing");

        let def = simple_def(vec![text_field("title"), text_field("body")]);
        index(&conn, &def);

        for (term, expected) in [
            ("jane@example.com", "1"),
            ("well-known", "1"),
            ("3.14", "2"),
            ("O'Brien", "2"),
        ] {
            assert_eq!(
                matching_ids(&conn, &def, term),
                vec![expected],
                "term {term}"
            );
        }
    }

    /// A single-locale search on `SQLite` confines every word to that locale's
    /// columns (and the non-localized ones) with an FTS5 column filter.
    #[test]
    fn a_locale_search_uses_a_column_filter() {
        let (_dir, conn) = setup_db();
        conn.execute_batch("CREATE TABLE _fts_posts (id TEXT)")
            .unwrap();

        let def = simple_def(vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            text_field("body"),
        ]);
        let locale = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: false,
            },
        };
        let search = FtsSearch::builder("posts", &def, "hallo welt")
            .locale_ctx(Some(&locale))
            .build();

        let (_, query) = fts_where_clause(&conn, &search, 1).unwrap().unwrap();

        assert_eq!(query, "{\"title__de\" \"body\"} : (\"hallo\" * \"welt\" *)");
    }
}
