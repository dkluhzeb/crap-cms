//! The views and triggers outside a rebuilt table that depend on it.
//!
//! A `SQLite` rebuild drops the table and renames its replacement into place.
//! The rename re-checks every view and trigger in the schema, and one that
//! names a table which no longer exists fails it ("error in view …: no such
//! table") — so a user view over the collection, or a trigger on another table
//! whose body writes to it, would stop the schema sync at boot. Each such
//! object is dropped before the old table goes and recreated from its stored
//! statement once the replacement carries the name again, inside the same
//! transaction.

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbValue, query::helpers::quote_ident};

/// A stored view or trigger: what it is, its name, the table (or view) it is
/// attached to, and its `CREATE` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SchemaObject {
    kind: String,
    name: String,
    table: String,
    sql: String,
}

impl SchemaObject {
    fn is_view(&self) -> bool {
        self.kind == "view"
    }

    /// Whether this object depends on `name`: attached to it, or naming it as
    /// an identifier in its statement.
    fn depends_on(&self, name: &str) -> bool {
        self.table.eq_ignore_ascii_case(name) || mentions_identifier(&self.sql, name)
    }
}

/// Whether `c` can be part of an unquoted `SQLite` identifier.
fn is_identifier_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || !c.is_ascii()
}

/// Whether `sql` names `name` as an identifier — bare, quoted or qualified
/// (`items`, `"items"`, `[items]`, `main.items`), ignoring ASCII case as
/// `SQLite` does — and not merely as part of a longer one (`items_tags`).
///
/// Conservative on purpose: a match inside a string literal or a comment also
/// counts. Such an object is dropped and recreated from its own statement
/// unchanged, which costs nothing; missing a real dependency fails the rebuild.
fn mentions_identifier(sql: &str, name: &str) -> bool {
    let haystack = sql.to_ascii_lowercase();
    let needle = name.to_ascii_lowercase();

    if needle.is_empty() {
        return false;
    }

    haystack.match_indices(&needle).any(|(at, _)| {
        let before = haystack[..at].chars().next_back();
        let after = haystack[at + needle.len()..].chars().next();

        !before.is_some_and(is_identifier_char) && !after.is_some_and(is_identifier_char)
    })
}

/// Every stored view, and every trigger not attached to `table` itself (those
/// go with the table and are recreated as its own objects), in creation order.
fn candidate_objects(conn: &dyn DbConnection, table: &str) -> Result<Vec<SchemaObject>> {
    let rows = conn
        .query_all(
            "SELECT type, name, tbl_name, sql FROM sqlite_master \
             WHERE type IN ('view', 'trigger') AND sql IS NOT NULL \
             AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             AND NOT (type = 'trigger' AND tbl_name = ?1 COLLATE NOCASE) \
             ORDER BY rowid",
            &[DbValue::Text(table.to_string())],
        )
        .with_context(|| format!("Failed to read the views and triggers around '{table}'"))?;

    rows.iter()
        .map(|row| -> Result<SchemaObject> {
            Ok(SchemaObject {
                kind: row.get_string("type")?,
                name: row.get_string("name")?,
                table: row.get_string("tbl_name")?,
                sql: row.get_string("sql")?,
            })
        })
        .collect()
}

/// Split off the objects of `pending` that depend on any of `names`.
fn take_dependents(pending: &mut Vec<SchemaObject>, names: &[String]) -> Vec<SchemaObject> {
    let (hit, rest): (Vec<_>, Vec<_>) = pending
        .drain(..)
        .partition(|object| names.iter().any(|name| object.depends_on(name)));

    *pending = rest;

    hit
}

/// The views and triggers outside `table` that depend on it — directly, or
/// through a view that does (a view over a view over the table, an
/// `INSTEAD OF` trigger on such a view) — views first, each kind in creation
/// order, so recreating them in turn attaches every trigger to an existing
/// view.
pub(super) fn dependent_objects(conn: &dyn DbConnection, table: &str) -> Result<Vec<SchemaObject>> {
    let mut pending = candidate_objects(conn, table)?;
    let mut names = vec![table.to_string()];
    let mut found = Vec::new();

    loop {
        let hit = take_dependents(&mut pending, &names);

        if hit.is_empty() {
            break;
        }

        names = hit
            .iter()
            .filter(|o| o.is_view())
            .map(|o| o.name.clone())
            .collect();
        found.extend(hit);
    }

    let (mut ordered, triggers): (Vec<_>, Vec<_>) =
        found.into_iter().partition(SchemaObject::is_view);
    ordered.extend(triggers);

    Ok(ordered)
}

/// Drop `objects` — triggers first, so none outlives the view it is attached to.
pub(super) fn drop_objects(conn: &dyn DbConnection, objects: &[SchemaObject]) -> Result<()> {
    for object in objects.iter().rev() {
        let kind = if object.is_view() { "VIEW" } else { "TRIGGER" };

        conn.execute_batch_ddl(&format!(
            "DROP {kind} IF EXISTS {}",
            quote_ident(&object.name)
        ))
        .with_context(|| format!("Failed to set aside {} '{}'", object.kind, object.name))?;
    }

    Ok(())
}

/// Recreate `objects` from their stored statements, in order.
pub(super) fn recreate_objects(conn: &dyn DbConnection, objects: &[SchemaObject]) -> Result<()> {
    for object in objects {
        conn.execute_batch_ddl(&object.sql)
            .with_context(|| format!("Failed to recreate {} '{}'", object.kind, object.name))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identifier_matches_bare_quoted_and_qualified_but_not_as_a_prefix() {
        for sql in [
            "SELECT * FROM items",
            "SELECT * FROM \"items\"",
            "SELECT * FROM [Items]",
            "SELECT * FROM main.ITEMS WHERE 1",
            "INSERT INTO items(id) VALUES (1)",
        ] {
            assert!(mentions_identifier(sql, "items"), "{sql}");
        }

        for sql in [
            "SELECT * FROM items_tags",
            "SELECT * FROM _rebuild_items",
            "SELECT * FROM line_items",
            "SELECT * FROM items2",
        ] {
            assert!(!mentions_identifier(sql, "items"), "{sql}");
        }
    }
}
