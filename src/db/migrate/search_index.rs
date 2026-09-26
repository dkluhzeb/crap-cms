//! The schema sync's upkeep of every collection's search index.
//!
//! The per-write upsert keeps an index current row by row, so the sync
//! rebuilds one only when its shape changed ([`fts_shape`]) — a rebuild drops
//! and repopulates the table inside the schema-sync transaction, holding a
//! lock that on Postgres blocks every other node's search and indexed write
//! for as long as it runs. A pass that rewrites stored values with plain
//! `UPDATE`s (which bypass the upsert) marks the collection's index stale with
//! [`invalidate`], and the sync rebuilds it once those passes are done.
//!
//! The gate is per collection — `fts_shape:{slug}` holding the fingerprint the
//! index was last built from.

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::Registry,
    db::{
        DbConnection,
        query::fts::{FtsIndex, fts_shape, fts_table_exists, sync_fts_table},
    },
};

use super::meta;

/// The `_crap_meta` key holding the shape `slug`'s index was built from.
fn gate_key(slug: &str) -> String {
    format!("fts_shape:{slug}")
}

/// Mark `slug`'s search index stale, so the next sync rebuilds it: its rows
/// were rewritten by a pass the per-write upsert never saw.
///
/// # Errors
///
/// Returns a backend error if the meta delete fails.
pub(super) fn invalidate(conn: &dyn DbConnection, slug: &str) -> Result<()> {
    meta::delete(conn, &gate_key(slug))
}

/// Rebuild the search index of every collection whose index shape changed
/// since it was built, or that a pass marked stale. A backend without full
/// text search has nothing to maintain.
///
/// # Errors
///
/// Returns an error if a shape can't be computed, or a backend error if a
/// rebuild or the meta upsert fails.
pub(super) fn sync_search_indexes(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    if !conn.supports_fts() {
        return Ok(());
    }

    for (slug, def) in &registry.collections {
        // Built with the registry, so rich text custom nodes contribute their
        // `searchable_attrs` exactly as the per-write upsert indexes them.
        let index = FtsIndex::builder(slug, def, locale_config)
            .registry(Some(registry))
            .build();

        sync_search_index(conn, &index)?;
    }

    Ok(())
}

/// Rebuild one index unless it was built from its current shape and its table
/// is where that shape says.
fn sync_search_index(conn: &dyn DbConnection, index: &FtsIndex<'_>) -> Result<()> {
    let key = gate_key(index.slug);
    let shape = fts_shape(conn, index)?;

    let built = meta::get(conn, &key)?.as_deref() == Some(shape.fingerprint.as_str());
    if built && shape.indexed == fts_table_exists(conn, index.slug) {
        return Ok(());
    }

    sync_fts_table(conn, index)?;

    meta::upsert(conn, &key, &shape.fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{CollectionDefinition, FieldDefinition, FieldType},
        db::{DbPool, migrate::collection::test_helpers::in_memory_pool},
    };

    fn registry(searchable: &[&str]) -> Registry {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("body", FieldType::Text).build(),
        ];
        def.admin.list_searchable_fields = searchable.iter().map(|f| (*f).to_string()).collect();

        let mut registry = Registry::new();
        registry.register_collection(def);

        registry
    }

    /// How many documents the index holds.
    fn indexed(pool: &DbPool) -> i64 {
        pool.get()
            .unwrap()
            .query_one("SELECT COUNT(*) FROM _fts_posts", &[])
            .unwrap()
            .and_then(|row| row.i64_at(0))
            .unwrap()
    }

    fn sync(pool: &DbPool, searchable: &[&str]) {
        let conn = pool.get().unwrap();

        sync_search_indexes(&conn, &registry(searchable), &LocaleConfig::default()).unwrap();
    }

    /// Regression: every start dropped and repopulated every search index
    /// inside the schema-sync transaction. An index whose shape is unchanged
    /// is left alone; a changed shape rebuilds it.
    #[test]
    fn an_unchanged_index_is_not_rebuilt() {
        let (_dir, pool) = in_memory_pool();
        pool.get()
            .unwrap()
            .execute_batch(
                "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, body TEXT); \
                 INSERT INTO posts (id, title, body) VALUES ('p0', 'Zero', 'x'), \
                 ('p1', 'Hello', 'World');",
            )
            .unwrap();

        sync(&pool, &["title"]);
        assert_eq!(indexed(&pool), 2);

        // A marker only a rebuild would undo: the index loses a document.
        pool.get()
            .unwrap()
            .execute("DELETE FROM _fts_posts WHERE id = 'p0'", &[])
            .unwrap();

        sync(&pool, &["title"]);
        assert_eq!(indexed(&pool), 1, "an unchanged index is kept");

        sync(&pool, &["title", "body"]);
        assert_eq!(indexed(&pool), 2, "a changed shape rebuilds the index");
    }

    /// A stale mark, or an index table gone missing, rebuilds the index.
    #[test]
    fn a_stale_or_missing_index_is_rebuilt() {
        let (_dir, pool) = in_memory_pool();
        pool.get()
            .unwrap()
            .execute_batch(
                "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, body TEXT); \
                 INSERT INTO posts (id, title) VALUES ('p1', 'Hello');",
            )
            .unwrap();

        sync(&pool, &["title"]);
        {
            let conn = pool.get().unwrap();
            conn.execute("UPDATE posts SET title = 'Rewritten'", &[])
                .unwrap();
            invalidate(&conn, "posts").unwrap();
        }

        sync(&pool, &["title"]);
        let title = pool
            .get()
            .unwrap()
            .query_one("SELECT title FROM _fts_posts WHERE id = 'p1'", &[])
            .unwrap()
            .and_then(|row| row.opt_text_at(0));
        assert_eq!(title.as_deref(), Some("Rewritten"));

        pool.get()
            .unwrap()
            .execute_batch("DROP TABLE _fts_posts")
            .unwrap();
        sync(&pool, &["title"]);
        assert_eq!(indexed(&pool), 1, "a missing index table is rebuilt");
    }
}
