//! The field values a document already holds, as its edit form shows them — the
//! stored row and its pending draft, each read for one locale with its rows.

use anyhow::{Context as _, Result};

use crate::{
    core::{DocumentFields, FieldDefinition},
    db::{
        DbConnection, DbValue, LocaleContext,
        ops::snapshot_read_document,
        query::{
            collect_column_names, find_latest_version, get_locale_select_columns_full,
            helpers::quote_ident, hydrate_document, read::decode_row,
        },
    },
};

/// The document [`find_stored_fields`] and [`find_pending_draft_fields`] read.
///
/// `table` is the table the document lives in (a collection slug or a global's
/// table); its join tables and version table are named from it.
pub(crate) struct StoredRow<'a> {
    pub table: &'a str,
    pub id: &'a str,
    pub fields: &'a [FieldDefinition],
    pub locale_ctx: Option<&'a LocaleContext>,
}

/// The select expressions for `fields`' columns alone — no system column — read
/// for the locale the way a document read resolves them.
fn field_select_exprs(
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<Vec<String>> {
    if let Some(ctx) = locale_ctx
        && ctx.config.is_enabled()
    {
        return Ok(get_locale_select_columns_full(fields, false, false, false, ctx)?.0);
    }

    let mut names = vec!["id".to_string()];
    collect_column_names(fields, &mut names);

    Ok(names.iter().map(|name| quote_ident(name)).collect())
}

/// The stored row's field values, decoded and hydrated like any document read:
/// groups nested, array and blocks rows included. `None` when there is no such
/// row.
///
/// # Errors
///
/// Returns a backend error if the SELECT or the hydration fails.
pub(crate) fn find_stored_fields(
    conn: &dyn DbConnection,
    row: &StoredRow<'_>,
) -> Result<Option<DocumentFields>> {
    let exprs = field_select_exprs(row.fields, row.locale_ctx)?;
    let sql = format!(
        "SELECT {} FROM \"{}\" WHERE id = {}",
        exprs.join(", "),
        row.table,
        conn.placeholder(1)
    );

    let found = conn
        .query_one(&sql, &[DbValue::Text(row.id.to_string())])
        .with_context(|| {
            format!(
                "Failed to read the stored fields of {} in {}",
                row.id, row.table
            )
        })?;

    let Some(found) = found else {
        return Ok(None);
    };

    let mut doc = decode_row(conn, &found, row.fields, row.locale_ctx)?;
    hydrate_document(conn, row.table, row.fields, &mut doc, None, row.locale_ctx)?;

    Ok(Some(doc.fields))
}

/// The document's pending draft — its latest version when that is a draft —
/// read like a draft read: decoded, resolved for the locale, groups nested.
/// `None` when the latest version is not a draft or there is none.
///
/// Only for a document whose drafts are kept as versions: the version table
/// must exist.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails, or an error if a configured
/// locale code has no column form.
pub(crate) fn find_pending_draft_fields(
    conn: &dyn DbConnection,
    row: &StoredRow<'_>,
) -> Result<Option<DocumentFields>> {
    let Some(version) = find_latest_version(conn, row.table, row.id)? else {
        return Ok(None);
    };

    if version.status != "draft" {
        return Ok(None);
    }

    let doc = snapshot_read_document(row.id, &version.snapshot, row.fields, row.locale_ctx)?;

    Ok(doc.map(|doc| doc.fields))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{core::FieldType, db::InMemoryConn};

    fn fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("summary", FieldType::Textarea).build(),
                ])
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                ])
                .build(),
        ]
    }

    fn conn() -> InMemoryConn {
        let conn = InMemoryConn::open();
        conn.setup(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, _revision INTEGER NOT NULL DEFAULT 0, title TEXT, seo__summary TEXT, \
             _status TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, \
             label TEXT);
             INSERT INTO posts VALUES ('p1', 0, 'Hello', 'Short', 'published', NULL, NULL);
             INSERT INTO posts_items VALUES ('r1', 'p1', 0, 'first');",
        );
        conn
    }

    /// The read carries every field — groups nested, rows hydrated — and only
    /// the fields: system columns are not selected.
    #[test]
    fn reads_the_row_with_groups_nested_and_rows_hydrated() {
        let conn = conn();
        let fields = fields();
        let row = StoredRow {
            table: "posts",
            id: "p1",
            fields: &fields,
            locale_ctx: None,
        };

        let stored = find_stored_fields(&conn, &row).unwrap().unwrap();

        assert_eq!(stored.get("title"), Some(&json!("Hello")));
        assert_eq!(stored.get("seo"), Some(&json!({ "summary": "Short" })));
        assert_eq!(stored["items"][0]["label"], json!("first"));
        assert!(stored.get("_status").is_none());
    }

    #[test]
    fn a_missing_row_reads_as_none() {
        let conn = conn();
        let fields = fields();
        let row = StoredRow {
            table: "posts",
            id: "nope",
            fields: &fields,
            locale_ctx: None,
        };

        assert!(find_stored_fields(&conn, &row).unwrap().is_none());
    }
}
