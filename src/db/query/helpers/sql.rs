//! SQL text building: identifier quoting, `LIKE` escaping, `WHERE` clause
//! assembly, the soft-delete predicate and placeholder lists.

use crate::{core::CollectionDefinition, db::DbConnection};

/// Escape the `LIKE` wildcards (`\`, `%`, `_`) in a value so it matches
/// literally under a `... LIKE ? ESCAPE '\'` clause. Backslash is escaped
/// first so the escapes this adds aren't re-escaped. Callers that interpolate
/// untrusted or wildcard-bearing text into a LIKE pattern MUST use this and
/// pair the query with `ESCAPE '\'`.
pub(crate) fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Quote a SQL identifier (column/table/index name) for interpolation into
/// DDL/DML. The one quoting there is — DDL, the `SELECT` list, and the
/// filter/sort/keyset comparand all emit identifiers through here.
///
/// Both `SQLite` and Postgres delimit identifiers with double quotes; an embedded
/// `"` is doubled per the SQL standard. Applied at every identifier-emission site
/// so a column whose name is a SQL reserved word — a user field legitimately
/// named `order`, `select`, `group`, … (allowed by field-name validation) — is
/// valid on every backend. (`SQLite`'s legacy "double-quoted string literal"
/// misfeature, which would otherwise turn a quoted *missing* column into a silent
/// string literal, is disabled per-connection via `SQLITE_DBCONFIG_DQS_*` — see
/// the pool setup — so a quoted identifier is always an identifier.)
///
/// Quoting is unconditional. Quoting only the names that *look* dangerous —
/// those with a capital, so Postgres wouldn't fold them — leaves the rest bare,
/// and a bare `user` on Postgres is the session-user function rather than the
/// column (a filter on it silently matches nothing), while `array`, `only`,
/// `grant` and `lateral` are outright syntax errors. `SQLite` accepts the bare
/// forms, so the whole class only ever showed on Postgres.
#[must_use]
pub(crate) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// [`quote_ident`] of `name`, qualified by `table` when one is given
/// (`"posts"."tags"`). A column read inside a subquery is qualified so a name
/// the subquery's own FROM item also exposes cannot shadow it.
#[must_use]
pub(crate) fn qualified_ident(table: Option<&str>, name: &str) -> String {
    let Some(table) = table else {
        return quote_ident(name);
    };

    format!("{}.{}", quote_ident(table), quote_ident(name))
}

/// Append a SQL condition with `WHERE` or `AND` depending on whether a WHERE clause already exists.
pub(crate) fn append_sql_condition(sql: &mut String, has_where: &mut bool, condition: &str) {
    sql.push_str(if *has_where { " AND " } else { " WHERE " });
    sql.push_str(condition);
    *has_where = true;
}

/// The predicate that selects live (non-trashed) rows: `_deleted_at IS NULL`.
///
/// One source for the soft-delete exclusion so every read path — the find
/// runner, the count / `max_updated_at` readers, the by-id read, and the auth
/// lookup — hides trash identically. If this ever changes (e.g. to a
/// scheduled-purge window), the by-id and login paths can't keep the old
/// semantics and resurrect trashed rows on one surface.
pub(crate) const SOFT_DELETE_ACTIVE: &str = "_deleted_at IS NULL";

/// Append the soft-delete exclusion [`SOFT_DELETE_ACTIVE`] when the collection
/// soft-deletes and the caller hasn't asked to include trashed rows. The single
/// decision point shared by the find runner and the count / `max_updated_at`
/// readers, so "when is trash hidden" can't drift between listing and counting.
pub(crate) fn append_soft_delete_filter(
    def: &CollectionDefinition,
    include_deleted: bool,
    sql: &mut String,
    has_where: &mut bool,
) {
    if def.soft_delete && !include_deleted {
        append_sql_condition(sql, has_where, SOFT_DELETE_ACTIVE);
    }
}

/// Build a comma-separated positional placeholder list — `?1, ?2, … ?N` on
/// `SQLite`, `$1, … $N` on Postgres — via [`DbConnection::placeholder`],
/// numbered from 1. Empty string when `count == 0`. One source for every
/// `IN (…)` / multi-value clause so the 1-based start and the backend dialect
/// can't drift per call site.
#[must_use]
pub(crate) fn placeholder_list(conn: &dyn DbConnection, count: usize) -> String {
    (1..=count)
        .map(|i| conn.placeholder(i))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "sqlite")]
    use crate::db::InMemoryConn;

    #[cfg(feature = "sqlite")]
    #[test]
    fn placeholder_list_numbers_from_one_and_empties_at_zero() {
        let conn = InMemoryConn::open();
        assert_eq!(placeholder_list(&conn, 0), "");
        assert_eq!(placeholder_list(&conn, 1), "?1");
        assert_eq!(placeholder_list(&conn, 3), "?1, ?2, ?3");
    }

    /// Every identifier is quoted, not just the ones with a capital Postgres
    /// would fold: a lowercase reserved word (`user`, `array`, `only`) left
    /// bare is a different expression on Postgres — `WHERE user = $1` reads
    /// the session-user function and matches nothing — or a syntax error.
    #[test]
    fn quote_ident_quotes_every_name_including_reserved_words() {
        assert_eq!(quote_ident("title__de"), "\"title__de\"");
        assert_eq!(quote_ident("title__de_DE"), "\"title__de_DE\"");

        for reserved in ["user", "array", "only", "grant", "lateral", "order"] {
            assert_eq!(quote_ident(reserved), format!("\"{reserved}\""));
        }

        // An embedded quote is doubled, per the SQL standard.
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn qualified_ident_prefixes_the_quoted_table() {
        assert_eq!(qualified_ident(None, "tags"), "\"tags\"");
        assert_eq!(qualified_ident(Some("posts"), "tags"), "\"posts\".\"tags\"");
    }
}
