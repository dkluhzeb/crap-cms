//! Tables no definition in the registry accounts for.
//!
//! The schema sync walks the registry, so a collection or global that was
//! renamed or removed leaves its table — and its versions and junction tables
//! — behind, invisible to every other migration pass. This module is the one
//! place that recognizes them, shared by the boot warning and by
//! `crap-cms db cleanup`, so the two can never disagree about what counts as a
//! leftover.
//!
//! Nothing here drops anything: a table that fell out of the registry may hold
//! the only copy of its data.

use std::collections::HashSet;

use anyhow::Result;
use tracing::warn;

use crate::{
    core::Registry,
    db::{
        DbConnection,
        migrate::helpers::get_table_columns,
        query::{
            helpers::{global_table, join_table, versions_table},
            join_field_names,
        },
    },
};

/// The system tables the scan never reports: the framework's own bookkeeping
/// (`_crap_*`) and the full-text index tables with their `SQLite` shadows
/// (`_fts_*`).
const SKIPPED_PREFIXES: &[&str] = &["_crap", "_fts_"];

/// What kind of leftover a table is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanKind {
    /// A collection table whose slug is no longer in the registry.
    Collection,
    /// A `_global_{slug}` table whose global is no longer in the registry.
    Global,
    /// A `_versions_{table}` table whose owner is no longer in the registry.
    Versions,
    /// A junction table (array, blocks, relationship) whose owning field is
    /// gone — or whose whole collection is.
    Junction,
}

impl OrphanKind {
    /// The word the report uses for this kind.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            OrphanKind::Collection => "collection table",
            OrphanKind::Global => "global table",
            OrphanKind::Versions => "versions table",
            OrphanKind::Junction => "junction table",
        }
    }
}

/// One table no definition accounts for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanTable {
    /// The table's name in the database.
    pub name: String,
    /// What the table was created as.
    pub kind: OrphanKind,
}

impl OrphanTable {
    /// Pair a table name with what kind of leftover it is.
    #[must_use]
    pub fn new(name: String, kind: OrphanKind) -> Self {
        Self { name, kind }
    }
}

/// Every table name the registry accounts for: each collection and global
/// table, their versions tables, and every junction table their fields imply.
///
/// A superset on purpose — a collection without versions names a versions
/// table it never got. Naming a table that doesn't exist costs nothing; failing
/// to name one that does would report a live table as a leftover. A has-one
/// reference names no junction: its values live in a column of the owning
/// table, so a junction left from the field's has-many past is a leftover.
fn known_tables(registry: &Registry) -> HashSet<String> {
    let mut known = HashSet::new();

    for (slug, def) in &registry.collections {
        known.insert(slug.to_string());
        known.insert(versions_table(slug));

        for field in join_field_names(&def.fields) {
            known.insert(join_table(slug, &field));
        }
    }

    for (slug, def) in &registry.globals {
        let table = global_table(slug);
        known.insert(versions_table(&table));

        for field in join_field_names(&def.fields) {
            known.insert(join_table(&table, &field));
        }

        known.insert(table);
    }

    known
}

/// Whether a table carries the shape every junction table has: a parent
/// pointer and a row order.
fn is_junction_shape(columns: &HashSet<String>) -> bool {
    columns.contains("parent_id") && columns.contains("_order")
}

/// Whether a table is one the scan never reports: a table the registry names,
/// or one of the framework's own.
fn is_accounted_for(name: &str, known: &HashSet<String>) -> bool {
    known.contains(name) || SKIPPED_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// What kind of leftover `name` is, or `None` when nothing says the table is
/// ours.
fn classify(name: &str, columns: &HashSet<String>) -> Option<OrphanKind> {
    if name.starts_with("_versions_") {
        return Some(OrphanKind::Versions);
    }

    if name.starts_with("_global_") {
        return Some(OrphanKind::Global);
    }

    if is_junction_shape(columns) {
        return Some(OrphanKind::Junction);
    }

    // The reference counter every collection table carries — the one marker
    // that tells a leftover collection apart from a table another application
    // put in the same database.
    if columns.contains("_ref_count") {
        return Some(OrphanKind::Collection);
    }

    None
}

/// Every table the registry no longer accounts for, sorted by name.
///
/// # Errors
///
/// Returns a backend error if listing the tables or reading their columns
/// fails.
pub fn find_orphan_tables(
    conn: &dyn DbConnection,
    registry: &Registry,
) -> Result<Vec<OrphanTable>> {
    let known = known_tables(registry);

    let mut names = conn.list_user_tables()?;
    names.sort();

    let mut orphans = Vec::new();

    for name in names {
        if is_accounted_for(&name, &known) {
            continue;
        }

        let columns = get_table_columns(conn, &name)?;

        if let Some(kind) = classify(&name, &columns) {
            orphans.push(OrphanTable::new(name, kind));
        }
    }

    Ok(orphans)
}

/// Warn once per leftover table at boot. Never drops one — `crap-cms db
/// cleanup --drop-tables` is where that decision is made, with the operator
/// looking at the list.
///
/// # Errors
///
/// Returns a backend error if the scan fails.
pub(super) fn warn_orphan_tables(conn: &dyn DbConnection, registry: &Registry) -> Result<()> {
    for orphan in find_orphan_tables(conn, registry)? {
        warn!(
            "Table '{}' ({}) belongs to no definition (not removed) — \
             `crap-cms db cleanup` lists it, `db cleanup --drop-tables -y` drops it",
            orphan.name,
            orphan.kind.label()
        );
    }

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::core::{
        CollectionDefinition, FieldDefinition, FieldType, RelationshipConfig, VersionsConfig,
    };
    use crate::db::migrate::collection::test_helpers::*;
    use crate::db::migrate::sync_all;

    /// A `posts` collection with an array field and versions, so a sync gives
    /// it a table, a junction table and a versions table.
    fn posts() -> CollectionDefinition {
        let mut def = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("items", FieldType::Array)
                    .fields(vec![text_field("label")])
                    .build(),
            ],
        );
        def.versions = Some(VersionsConfig::new(true, 10));

        def
    }

    fn registry_with_posts() -> Registry {
        let mut registry = Registry::new();
        registry.register_collection(posts());

        registry
    }

    fn orphan_names(conn: &dyn DbConnection, registry: &Registry) -> Vec<String> {
        find_orphan_tables(conn, registry)
            .unwrap()
            .into_iter()
            .map(|o| o.name)
            .collect()
    }

    /// A collection removed from the registry leaves three tables behind —
    /// its own, its junction table and its versions table — and every
    /// registry-driven pass walks straight past them. The scan names all
    /// three, and leaves the live collection's tables alone.
    #[test]
    fn a_removed_collection_leaves_every_one_of_its_tables_behind() {
        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &registry_with_posts(), &no_locale()).unwrap();

        let conn = pool.get().unwrap();

        assert!(
            orphan_names(&conn, &registry_with_posts()).is_empty(),
            "a synced registry accounts for its own tables"
        );

        let names = orphan_names(&conn, &Registry::new());

        for expected in ["posts", "posts_items", "_versions_posts"] {
            assert!(
                names.contains(&expected.to_string()),
                "{expected} must be reported: {names:?}"
            );
        }
    }

    /// A field removed from a live collection leaves its junction table, which
    /// is reported on its own — the collection's other tables are fine.
    #[test]
    fn a_removed_field_leaves_its_junction_table() {
        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &registry_with_posts(), &no_locale()).unwrap();

        let conn = pool.get().unwrap();

        let mut without_array = Registry::new();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.versions = Some(VersionsConfig::new(true, 10));
        without_array.register_collection(def);

        assert_eq!(orphan_names(&conn, &without_array), vec!["posts_items"]);
    }

    /// A relationship turned from has-many to has-one stores its value in the
    /// owning table's column; the junction its has-many past left is reported
    /// like any other leftover, so `db cleanup` can drop it.
    #[test]
    fn a_has_one_reference_leaves_its_old_junction_reported() {
        let (_dir, pool) = in_memory_pool();

        let posts = |has_many: bool| {
            let mut registry = Registry::new();
            registry.register_collection(simple_collection("tags", vec![]));
            registry.register_collection(simple_collection(
                "posts",
                vec![
                    FieldDefinition::builder("tags", FieldType::Relationship)
                        .relationship(RelationshipConfig::new("tags", has_many))
                        .build(),
                ],
            ));

            registry
        };

        sync_all(&pool, &posts(true), &no_locale()).unwrap();
        let conn = pool.get().unwrap();

        assert!(orphan_names(&conn, &posts(true)).is_empty());
        assert_eq!(orphan_names(&conn, &posts(false)), vec!["posts_tags"]);
    }

    /// A global removed from the registry leaves its `_global_` table.
    #[test]
    fn a_removed_global_leaves_its_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE _global_site (id TEXT PRIMARY KEY, title TEXT, _ref_count INTEGER)",
        )
        .unwrap();

        assert_eq!(orphan_names(&conn, &Registry::new()), vec!["_global_site"]);
    }

    /// The framework's own tables are never leftovers, whatever the registry
    /// holds.
    #[test]
    fn system_tables_are_never_reported() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        assert!(
            orphan_names(&conn, &Registry::new()).is_empty(),
            "a database with nothing but system tables is clean"
        );
    }

    /// A table another application put in the same database carries none of
    /// the markers, so it is left out of the report entirely.
    #[test]
    fn a_foreign_table_is_not_reported() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute_batch("CREATE TABLE billing_invoices (id TEXT PRIMARY KEY, total REAL)")
            .unwrap();

        assert!(
            orphan_names(&conn, &Registry::new()).is_empty(),
            "only crap-shaped tables are reported"
        );
    }
}
