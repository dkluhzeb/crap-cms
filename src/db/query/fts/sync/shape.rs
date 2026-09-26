//! The shape of a collection's search index — everything its table is built
//! from — so the schema sync rebuilds the index only when that changes.
//!
//! Every write keeps an index current row by row (the per-write upsert), so a
//! rebuild is needed only when what the index holds changes: the indexed
//! columns, the locale columns they expand to, the Postgres tsvectors over
//! them, the rich text columns read as their text and the custom-node attrs
//! that text includes, the backend and its text-search configuration.

use anyhow::Result;

use crate::db::DbConnection;
use crate::db::query::fts::fields::{RichtextFormat, build_node_searchable_map, richtext_columns};
use crate::db::query::fts::index::FtsIndex;
use crate::db::query::fts::layout::{PG_FTS_CONFIG, fts_columns, pg_vectors};
use crate::db::query::fts::search::{fts_table_name, table_exists};

/// Leads every fingerprint. Bump it whenever the way an index is built
/// changes (the text a column contributes, the table layout), so every index
/// is rebuilt once on the next start.
const INDEX_VERSION: &str = "1";

/// The shape of one collection's search index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FtsShape {
    /// Everything the index is built from, as one comparable string.
    pub fingerprint: String,
    /// Whether the index has any column — without one it has no table.
    pub indexed: bool,
}

impl FtsShape {
    fn new(fingerprint: String, indexed: bool) -> Self {
        Self {
            fingerprint,
            indexed,
        }
    }
}

/// The shape of `index`'s search index on `conn`'s backend.
///
/// # Errors
///
/// Returns an error if a field name is not a plain identifier or a locale code
/// has no column form — the same checks building the index runs.
pub fn fts_shape(conn: &dyn DbConnection, index: &FtsIndex<'_>) -> Result<FtsShape> {
    let columns = fts_columns(index.def, index.locale_config)?;

    let mut parts = vec![
        INDEX_VERSION.to_string(),
        conn.kind().to_string(),
        PG_FTS_CONFIG.to_string(),
    ];

    parts.extend(
        columns
            .iter()
            .map(|c| format!("col {}={}", c.name, c.read_expr)),
    );

    if conn.is_postgres() {
        let vectors = pg_vectors(&columns, index.locale_config)?;
        parts.extend(
            vectors
                .iter()
                .map(|v| format!("tsv {}={:?}", v.name, v.members)),
        );
    }

    parts.extend(richtext_parts(index));

    Ok(FtsShape::new(parts.join("|"), !columns.is_empty()))
}

/// The rich text columns and the custom-node attrs their text includes,
/// sorted — both are read from hash maps.
fn richtext_parts(index: &FtsIndex<'_>) -> Vec<String> {
    let mut parts: Vec<String> = richtext_columns(index.def)
        .into_iter()
        .map(|(column, format)| {
            let format = match format {
                RichtextFormat::Html => "html",
                RichtextFormat::Json => "json",
            };

            format!("richtext {column}={format}")
        })
        .collect();

    parts.extend(
        build_node_searchable_map(index.def, index.registry)
            .into_iter()
            .map(|(node, attrs)| format!("node {node}={}", attrs.join(","))),
    );

    parts.sort();

    parts
}

/// Whether `slug`'s search index table exists.
#[must_use]
pub fn fts_table_exists(conn: &dyn DbConnection, slug: &str) -> bool {
    table_exists(conn, &fts_table_name(slug))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;
    use crate::db::query::fts::sync::test_helpers::*;

    fn shape(conn: &dyn DbConnection, fields: &[&str]) -> FtsShape {
        let mut def = simple_def(vec![text_field("title"), text_field("body")]);
        def.admin.list_searchable_fields = fields.iter().map(|f| (*f).to_string()).collect();

        fts_shape(
            conn,
            &FtsIndex::builder("posts", &def, &LocaleConfig::default()).build(),
        )
        .unwrap()
    }

    /// The same definition has the same shape; indexing another column
    /// changes it.
    #[test]
    fn the_shape_follows_the_indexed_columns() {
        let (_dir, conn) = setup_db();

        assert_eq!(shape(&conn, &["title"]), shape(&conn, &["title"]));
        assert_ne!(
            shape(&conn, &["title"]).fingerprint,
            shape(&conn, &["title", "body"]).fingerprint
        );
        assert!(shape(&conn, &["title"]).indexed);
    }
}
