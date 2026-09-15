//! SQL text building: identifier quoting, `LIKE` escaping, `WHERE` clause
//! assembly, the soft-delete predicate and placeholder lists.

use std::borrow::Cow;

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

/// Quote a SQL identifier (column/table name) for interpolation into DDL/DML.
///
/// Both `SQLite` and Postgres delimit identifiers with double quotes; an embedded
/// `"` is doubled per the SQL standard. Applied at every identifier-emission site
/// so a column whose name is a SQL reserved word — a user field legitimately
/// named `order`, `select`, `group`, … (allowed by field-name validation) — is
/// valid on every backend. (`SQLite`'s legacy "double-quoted string literal"
/// misfeature, which would otherwise turn a quoted *missing* column into a silent
/// string literal, is disabled per-connection via `SQLITE_DBCONFIG_DQS_*` — see
/// the pool setup — so a quoted identifier is always an identifier.)
#[must_use]
pub(crate) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Render a generated identifier for SQL: quoted when it contains a capital
/// letter, bare otherwise. Postgres folds an unquoted identifier to lowercase,
/// so a column named after a locale code with capitals (`title__de_DE`) would
/// otherwise refer to a column that doesn't exist.
pub(crate) fn sql_ident(name: &str) -> Cow<'_, str> {
    if name.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(quote_ident(name))
    } else {
        Cow::Borrowed(name)
    }
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

    /// A generated name with capitals (a `de_DE` locale suffix) is quoted, so
    /// Postgres doesn't fold it to a column that doesn't exist.
    #[test]
    fn sql_ident_quotes_only_names_with_capitals() {
        assert_eq!(sql_ident("title__de"), "title__de");
        assert_eq!(sql_ident("title__de_DE"), "\"title__de_DE\"");
    }
}
