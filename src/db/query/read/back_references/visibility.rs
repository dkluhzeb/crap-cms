//! Access-visibility filtering for back-references.
//!
//! The raw scan ([`super::find_back_references`]) finds every document that
//! references a target, ignoring access. Before those rows are shown to a
//! viewer they are intersected with the viewer's *visibility filter* — the
//! compiled status/lifecycle/access predicate for the owner collection (built
//! in the service layer from the same view-scope primitive that gates normal
//! reads). [`filter_visible_ids`] runs that intersection in SQL so a referrer
//! the viewer could not read never appears in the back-reference list.

use std::collections::HashSet;

use anyhow::{Context as _, Result};

use crate::core::CollectionDefinition;
use crate::db::query::filter::{build_where_clause, resolve_filters};
use crate::db::{DbConnection, DbValue, Filter, FilterClause, FilterOp, LocaleContext};

/// Maximum candidate ids per `IN (…)` chunk. Kept well under `SQLite`'s 999
/// bind-variable limit so the visibility filter's own params still fit.
const ID_CHUNK: usize = 400;

/// Return the subset of `candidate_ids` that the viewer may see, by
/// intersecting them with `visibility` — the compiled status/lifecycle/access
/// filter for `def`.
///
/// `visibility` must be non-empty: an empty filter means "everything visible",
/// which the caller short-circuits without a query. Candidate ids are chunked
/// to stay within the backend's bind-variable limit.
///
/// # Errors
///
/// Returns a backend error if a visibility query fails, or a filter error if
/// `visibility` references an unresolvable field.
pub fn filter_visible_ids(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    candidate_ids: &[String],
    visibility: &[FilterClause],
    locale_ctx: Option<&LocaleContext>,
) -> Result<HashSet<String>> {
    let mut visible = HashSet::with_capacity(candidate_ids.len());

    for chunk in candidate_ids.chunks(ID_CHUNK) {
        let mut clauses = Vec::with_capacity(visibility.len() + 1);
        clauses.push(FilterClause::Single(Filter {
            field: "id".to_string(),
            op: FilterOp::In(chunk.to_vec()),
        }));
        clauses.extend(visibility.iter().cloned());

        let resolved = resolve_filters(&clauses, def, locale_ctx)?;

        let mut params: Vec<DbValue> = Vec::new();
        let where_clause =
            build_where_clause(conn, &resolved, slug, &def.fields, locale_ctx, &mut params)?;
        let sql = format!("SELECT id FROM \"{slug}\"{where_clause}");

        let rows = conn
            .query_all(&sql, &params)
            .with_context(|| format!("Back-ref visibility query failed on '{slug}'"))?;

        for row in &rows {
            visible.insert(row.get_string("id")?);
        }
    }

    Ok(visible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldDefinition, FieldType};
    use crate::db::query::read::back_references::test_helpers::*;

    /// A `_status = 'published'` guard clause.
    fn status_published() -> FilterClause {
        FilterClause::Single(Filter {
            field: "_status".to_string(),
            op: FilterOp::Equals("published".to_string()),
        })
    }

    /// An `author = <id>` access constraint.
    fn author_is(id: &str) -> FilterClause {
        FilterClause::Single(Filter {
            field: "author".to_string(),
            op: FilterOp::Equals(id.to_string()),
        })
    }

    fn posts_with_author() -> CollectionDefinition {
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![FieldDefinition::builder("author", FieldType::Text).build()];
        posts.versions = Some(crate::core::collection::VersionsConfig::new(true, 0));
        posts
    }

    #[test]
    fn status_filter_drops_non_matching_rows() {
        let posts = posts_with_author();
        let (_tmp, pool, _registry) = setup_db(std::slice::from_ref(&posts), &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute(
            "INSERT INTO posts (id, _status) VALUES ('p1', 'published')",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts (id, _status) VALUES ('p2', 'draft')",
            &[],
        )
        .unwrap();

        let ids = vec!["p1".to_string(), "p2".to_string()];
        let visible =
            filter_visible_ids(&conn, "posts", &posts, &ids, &[status_published()], None).unwrap();

        assert_eq!(visible.len(), 1);
        assert!(visible.contains("p1"));
        assert!(!visible.contains("p2"));
    }

    #[test]
    fn access_constraint_drops_other_owners_rows() {
        let posts = posts_with_author();
        let (_tmp, pool, _registry) = setup_db(std::slice::from_ref(&posts), &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute(
            "INSERT INTO posts (id, _status, author) VALUES ('p1', 'published', 'me')",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts (id, _status, author) VALUES ('p2', 'published', 'you')",
            &[],
        )
        .unwrap();

        let ids = vec!["p1".to_string(), "p2".to_string()];
        let visibility = vec![status_published(), author_is("me")];
        let visible = filter_visible_ids(&conn, "posts", &posts, &ids, &visibility, None).unwrap();

        assert_eq!(visible.len(), 1);
        assert!(visible.contains("p1"));
    }

    #[test]
    fn empty_candidates_yields_empty() {
        let posts = posts_with_author();
        let (_tmp, pool, _registry) = setup_db(std::slice::from_ref(&posts), &[], &no_locale());
        let conn = pool.get().unwrap();

        let visible =
            filter_visible_ids(&conn, "posts", &posts, &[], &[status_published()], None).unwrap();
        assert!(visible.is_empty());
    }

    #[test]
    fn chunking_handles_more_than_one_batch() {
        let posts = posts_with_author();
        let (_tmp, pool, _registry) = setup_db(std::slice::from_ref(&posts), &[], &no_locale());
        let conn = pool.get().unwrap();

        // Insert more rows than a single chunk so the helper must paginate.
        let total = ID_CHUNK + 50;
        let mut ids = Vec::with_capacity(total);
        for i in 0..total {
            let id = format!("p{i}");
            conn.execute(
                "INSERT INTO posts (id, _status) VALUES (?1, 'published')",
                &[DbValue::Text(id.clone())],
            )
            .unwrap();
            ids.push(id);
        }

        let visible =
            filter_visible_ids(&conn, "posts", &posts, &ids, &[status_published()], None).unwrap();
        assert_eq!(
            visible.len(),
            total,
            "every published id across both chunks"
        );
    }
}
