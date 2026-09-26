//! Constraint changes on an existing collection table that ADD COLUMN cannot
//! express, and the `SQLite` table rebuild that carries them out.
//!
//! Three exist: the `soft_delete` transition drops the inline `UNIQUE`
//! constraints the partial unique indexes replace, the one-time pass drops the
//! inline `UNIQUE` older releases put on unique fields (the managed unique
//! indexes replace it), and the one-time relax drops the `NOT NULL` older
//! releases put on required user-field columns.
//! Postgres does both in place. `SQLite` can do neither, so the table is
//! rebuilt from the current definition — which declares neither constraint —
//! built under a temporary name, filled, then swapped in, in the one order
//! that leaves every child table's foreign key pointing at the collection.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result, bail};
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::CollectionDefinition,
    db::{
        DbConnection, DbValue,
        migrate::{
            helpers::{get_table_column_types, get_table_columns},
            inline_unique::{inline_unique_to_drop, mark_dropped},
            nullable_columns::{columns_to_relax, drop_not_null, mark_relaxed},
        },
        query::helpers::quote_ident,
    },
};

use super::{
    create::create_collection_table,
    indexes::index_prefix,
    rebuild_dependents::{dependent_objects, drop_objects, recreate_objects},
    soft_delete::{drop_unique_constraints, soft_delete_transition_pending},
};

/// The constraint changes an existing collection table still needs.
pub(in crate::db::migrate) struct PendingConstraints {
    /// The `soft_delete` transition: inline `UNIQUE` constraints to drop.
    drop_unique: bool,
    /// Whether the table still carries an inline `UNIQUE` an older release
    /// created — `None` once the one-time pass ran for this collection.
    legacy_unique: Option<bool>,
    /// User-field columns still `NOT NULL` — `None` once the one-time relax ran
    /// for this collection.
    relax_not_null: Option<Vec<String>>,
}

impl PendingConstraints {
    /// Read what `slug`'s stored table (with columns `existing`) still needs.
    ///
    /// The one reading of that condition: the schema sync asks it up front
    /// too, because on `SQLite` a rebuild can only run with foreign keys off,
    /// and that pragma has to be set before the sync transaction opens.
    pub(in crate::db::migrate) fn read(
        conn: &dyn DbConnection,
        slug: &str,
        def: &CollectionDefinition,
        existing: &HashSet<String>,
        locale_config: &LocaleConfig,
    ) -> Result<Self> {
        Ok(Self {
            drop_unique: soft_delete_transition_pending(def, existing, locale_config),
            legacy_unique: inline_unique_to_drop(conn, slug)?,
            relax_not_null: columns_to_relax(conn, slug)?,
        })
    }

    /// Whether the table's inline `UNIQUE` constraints are dropped.
    fn drops_unique(&self) -> bool {
        self.drop_unique || self.legacy_unique == Some(true)
    }

    /// Whether carrying the changes out on `SQLite` rebuilds the table.
    pub(in crate::db::migrate) fn needs_rebuild(&self) -> bool {
        self.drops_unique() || self.relax_not_null.as_ref().is_some_and(|c| !c.is_empty())
    }

    /// Carry the changes out on `slug`, then stamp the relax gate.
    pub(super) fn apply(
        &self,
        conn: &dyn DbConnection,
        slug: &str,
        def: &CollectionDefinition,
        locale_config: &LocaleConfig,
    ) -> Result<()> {
        if self.drops_unique() {
            info!("Removing inline UNIQUE constraints from '{slug}'");
        }

        if let Some(columns) = self.relax_not_null.as_ref().filter(|c| !c.is_empty()) {
            info!(
                "Dropping NOT NULL from '{slug}' columns: {}",
                columns.join(", ")
            );
        }

        if conn.is_postgres() {
            self.apply_in_place(conn, slug)?;
        } else if self.needs_rebuild() {
            rebuild_table(conn, slug, def, locale_config)?;
        }

        if self.legacy_unique.is_some() {
            mark_dropped(conn, slug)?;
        }

        if self.relax_not_null.is_some() {
            mark_relaxed(conn, slug)?;
        }

        Ok(())
    }

    /// Postgres: drop the constraints in place, leaving the table — and every
    /// foreign key pointing at it — alone.
    fn apply_in_place(&self, conn: &dyn DbConnection, slug: &str) -> Result<()> {
        if self.drops_unique() {
            drop_unique_constraints(conn, slug)?;
        }

        match &self.relax_not_null {
            Some(columns) => drop_not_null(conn, slug, columns),
            None => Ok(()),
        }
    }
}

/// The quoted, comma-separated list of columns present in both the old and the
/// new table — the copy list of a rebuild, in a deterministic order (the sets
/// are hashed, and the same string has to serve as the INSERT list and the
/// SELECT list).
fn sorted_quoted_columns(old_cols: &HashSet<String>, new_cols: &HashSet<String>) -> String {
    let mut common: Vec<&String> = old_cols.intersection(new_cols).collect();
    common.sort();

    common
        .iter()
        .map(|c| quote_ident(c.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The temporary name a rebuild assembles the replacement table under.
fn rebuild_table_name(slug: &str) -> String {
    format!("_rebuild_{slug}")
}

/// Rebuild `slug` from the current definition, in the order `SQLite`
/// documents: build the replacement under a temporary name, copy into it, drop
/// the original, rename the replacement into its place.
///
/// The order is the whole point. Renaming the original out of the way first
/// rewrites every child's `REFERENCES` clause to the temporary name — `SQLite`
/// does that whenever foreign keys are enabled — and dropping that table then
/// runs an implicit `DELETE FROM`, which fires the children's `ON DELETE
/// CASCADE` and takes the junction and version rows with it.
///
/// Both the drop and the rename here are only correct with foreign-key
/// enforcement off: off, the drop cascades nothing and the rename leaves the
/// children pointing at `slug`, which the renamed replacement then *is*. The
/// pragma is ignored inside a transaction, so `migrate::sync` opens that window
/// around the sync transaction and verifies the result with
/// `PRAGMA foreign_key_check` before committing.
///
/// `create_collection_table` creates no indexes, so the temporary table carries
/// none to collide with the managed `idx_{slug}_…` names; the schema sync
/// creates them on the renamed table afterwards. Every other index and trigger on the
/// table — one a Lua migration or an operator created — goes with the dropped
/// table, so it is recreated from its stored definition once the replacement
/// is in place. The views and triggers elsewhere that depend on the table
/// would fail the rename, so they are set aside before the drop and recreated
/// after it (see [`dependent_objects`]). The caller has already checked every
/// column the definition names against the stored type, so the replacement's
/// columns hold the same types as the ones they are copied from.
fn rebuild_table(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    refuse_cascading_rebuild(conn, slug)?;

    let temp = rebuild_table_name(slug);
    let old_types = get_table_column_types(conn, slug)?;
    let unmanaged = unmanaged_schema_objects(conn, slug)?;
    let dependents = dependent_objects(conn, slug)?;

    conn.execute_batch_ddl(&format!("DROP TABLE IF EXISTS \"{temp}\""))?;
    create_collection_table(conn, &temp, def, locale_config)?;

    if let Err(e) = fill_rebuilt_table(conn, slug, &temp, &old_types) {
        // Nothing has been dropped yet: discard the half-filled replacement and
        // leave the original table exactly as it was.
        let _ = conn.execute_batch_ddl(&format!("DROP TABLE IF EXISTS \"{temp}\""));

        return Err(e);
    }

    drop_objects(conn, &dependents)?;

    conn.execute_batch_ddl(&format!("DROP TABLE \"{slug}\""))
        .with_context(|| format!("Failed to drop the old table during rebuild of '{slug}'"))?;

    conn.execute_batch_ddl(&format!("ALTER TABLE \"{temp}\" RENAME TO \"{slug}\""))
        .with_context(|| format!("Failed to rename the rebuilt table into '{slug}'"))?;

    for sql in &unmanaged {
        conn.execute_batch_ddl(sql)
            .with_context(|| format!("Failed to recreate on the rebuilt '{slug}': {sql}"))?;
    }

    recreate_objects(conn, &dependents)?;

    info!("Table '{slug}' rebuilt successfully");

    Ok(())
}

/// Refuse a rebuild that would cascade into the tables pointing at `slug`.
///
/// With foreign-key enforcement on, dropping the old table deletes every
/// child row that points at it (see [`rebuild_table`]). The schema sync turns
/// enforcement off before its transaction whenever it finds a rebuild
/// pending; this is the backstop for a rebuild that runs outside that window,
/// which must fail rather than empty the junction and version tables.
fn refuse_cascading_rebuild(conn: &dyn DbConnection, slug: &str) -> Result<()> {
    let enforced = conn
        .query_one("PRAGMA foreign_keys", &[])
        .context("Failed to read the foreign-key setting")?
        .and_then(|row| row.i64_at(0))
        .is_some_and(|on| on != 0);

    if !enforced {
        return Ok(());
    }

    let child = conn
        .query_one(
            "SELECT m.name AS name FROM sqlite_master m, pragma_foreign_key_list(m.name) f \
             WHERE m.type = 'table' AND f.\"table\" = ?1 COLLATE NOCASE LIMIT 1",
            &[DbValue::Text(slug.to_string())],
        )
        .with_context(|| format!("Failed to read the tables referencing '{slug}'"))?;

    let Some(child) = child.and_then(|row| row.get_string("name").ok()) else {
        return Ok(());
    };

    bail!(
        "Refusing to rebuild '{slug}' with foreign keys enforced: dropping it would delete the \
         rows of '{child}' that point at it"
    )
}

/// The `CREATE` statements of the indexes and triggers on `slug` that the
/// schema sync does not manage: everything but the `idx_{slug}_…` indexes
/// the schema sync creates and the automatic indexes of inline constraints,
/// which have no statement.
fn unmanaged_schema_objects(conn: &dyn DbConnection, slug: &str) -> Result<Vec<String>> {
    let managed = index_prefix(slug);

    let rows = conn
        .query_all(
            "SELECT name, sql FROM sqlite_master \
             WHERE tbl_name = ?1 AND type IN ('index', 'trigger') AND sql IS NOT NULL \
             ORDER BY type, name",
            &[DbValue::Text(slug.to_string())],
        )
        .with_context(|| format!("Failed to read the indexes and triggers of '{slug}'"))?;

    Ok(rows
        .iter()
        .filter(|row| {
            row.get_string("name")
                .is_ok_and(|name| !name.starts_with(&managed))
        })
        .filter_map(|row| row.get_string("sql").ok())
        .collect())
}

/// Carry the old table's data into the replacement.
fn fill_rebuilt_table(
    conn: &dyn DbConnection,
    slug: &str,
    temp: &str,
    old_types: &HashMap<String, String>,
) -> Result<()> {
    add_leftover_columns(conn, temp, old_types)?;

    let old_cols: HashSet<String> = old_types.keys().cloned().collect();
    let new_cols = get_table_columns(conn, temp)?;
    let col_list = sorted_quoted_columns(&old_cols, &new_cols);

    conn.execute(
        &format!("INSERT INTO \"{temp}\" ({col_list}) SELECT {col_list} FROM \"{slug}\""),
        &[],
    )
    .with_context(|| format!("Failed to copy data during rebuild of '{slug}'"))?;

    Ok(())
}

/// Re-add every column the old table has and the definition no longer does —
/// a removed field's column, a system column of a feature turned off — with
/// its stored type and without any constraint.
///
/// The normal alter path preserves such columns (it warns, it never drops), so
/// the rebuild must not silently destroy their data either.
fn add_leftover_columns(
    conn: &dyn DbConnection,
    temp: &str,
    old_types: &HashMap<String, String>,
) -> Result<()> {
    let fresh_cols = get_table_columns(conn, temp)?;

    let mut leftovers: Vec<(&String, &String)> = old_types
        .iter()
        .filter(|(col, _)| !fresh_cols.contains(*col))
        .collect();
    leftovers.sort();

    for (col, col_type) in leftovers {
        conn.execute_batch_ddl(&format!(
            "ALTER TABLE \"{temp}\" ADD COLUMN {} {col_type}",
            quote_ident(col)
        ))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{FieldDefinition, FieldType, Registry, VersionsConfig},
        db::{
            DbPool, DbValue,
            migrate::{
                collection::{sync_collection_table, test_helpers::*},
                sync_all,
            },
        },
    };

    /// Regression: the rebuild's copy list joined the column names bare, so a
    /// locale column carrying the locale code's capitals (`title__de_DE`) was
    /// folded to lowercase by Postgres — the `INSERT … SELECT` named a column
    /// that does not exist and the migration aborted at boot. Only columns in
    /// both tables are copied, and the one list serves as both the INSERT and
    /// the SELECT list, so it is built once and ordered deterministically.
    #[test]
    fn the_rebuild_copy_list_quotes_every_column() {
        let old_cols: HashSet<String> = ["id", "title__de_DE", "title__en", "dropped"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let new_cols: HashSet<String> = ["id", "title__de_DE", "title__en", "added"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        assert_eq!(
            sorted_quoted_columns(&old_cols, &new_cols),
            "\"id\", \"title__de_DE\", \"title__en\"",
            "columns present in both tables, each quoted, in a stable order"
        );
    }

    /// The rebuild carries the table's rows across and leaves no temporary
    /// behind.
    ///
    /// Nothing is dropped until the copy has succeeded, so a failing copy
    /// discards the half-filled replacement and leaves the original exactly as
    /// it was — there is no window in which the data lives only in a table the
    /// rebuild is still assembling.
    #[test]
    fn rebuild_preserves_rows_and_leaves_no_temporary() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        // Create a table with a unique constraint (simulates pre-soft_delete state)
        conn.execute(
            "CREATE TABLE items (id TEXT PRIMARY KEY, title TEXT UNIQUE, created_at TEXT, updated_at TEXT, _ref_count INTEGER DEFAULT 0)",
            &[],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO items (id, title) VALUES ('1', 'Hello'), ('2', 'World')",
            &[],
        )
        .unwrap();

        let mut def = simple_collection("items", vec![text_field("title")]);
        def.soft_delete = true;

        rebuild_table(&conn, "items", &def, &no_locale()).unwrap();

        let rows = conn
            .query_all("SELECT id, title FROM items ORDER BY id", &[])
            .unwrap();
        assert_eq!(rows.len(), 2, "both rows should survive rebuild");

        let temp_exists = conn
            .query_one(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='_rebuild_items'",
                &[],
            )
            .unwrap();
        assert!(temp_exists.is_none(), "temp table should be dropped");
    }

    /// A column the definition no longer names keeps its data and its stored
    /// type through a rebuild — a system column of a feature turned off too,
    /// which the normal alter path would also have left in place.
    #[test]
    fn rebuild_keeps_leftover_columns_with_their_type() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE items (id TEXT PRIMARY KEY, title TEXT NOT NULL, \
             legacy INTEGER NOT NULL, _status TEXT NOT NULL DEFAULT 'draft', \
             _ref_count INTEGER NOT NULL DEFAULT 0, created_at TEXT, updated_at TEXT);
             INSERT INTO items (id, title, legacy) VALUES ('1', 'Hello', 7);",
        )
        .unwrap();

        let def = simple_collection("items", vec![text_field("title")]);
        rebuild_table(&conn, "items", &def, &no_locale()).unwrap();

        let types = get_table_column_types(&conn, "items").unwrap();
        assert_eq!(types.get("legacy").map(String::as_str), Some("INTEGER"));

        let row = conn
            .query_one("SELECT legacy, _status FROM items WHERE id = '1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64("legacy").unwrap(), 7);
        assert_eq!(row.get_string("_status").unwrap(), "draft");

        conn.execute("INSERT INTO items (id) VALUES ('2')", &[])
            .expect("a leftover column must not keep its NOT NULL");
    }

    /// Regression: the rebuild dropped every index and trigger on the table
    /// with it, and only the managed `idx_{slug}_…` indexes came back — a
    /// unique index or trigger a Lua migration had created vanished without a
    /// word, silently lifting the constraint. Each is recreated from its
    /// stored definition.
    #[test]
    fn rebuild_keeps_unmanaged_indexes_and_triggers() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE items (id TEXT PRIMARY KEY, title TEXT NOT NULL, \
             _ref_count INTEGER NOT NULL DEFAULT 0, created_at TEXT, updated_at TEXT);
             CREATE UNIQUE INDEX items_title_custom ON items (title);
             CREATE TABLE audit (item TEXT);
             CREATE TRIGGER items_audit AFTER INSERT ON items \
                 BEGIN INSERT INTO audit (item) VALUES (NEW.id); END;
             INSERT INTO items (id, title) VALUES ('1', 'Hello');",
        )
        .unwrap();

        let def = simple_collection("items", vec![text_field("title")]);
        rebuild_table(&conn, "items", &def, &no_locale()).unwrap();

        conn.execute("INSERT INTO items (id, title) VALUES ('2', 'Hello')", &[])
            .expect_err("the custom unique index must survive the rebuild");

        conn.execute("INSERT INTO items (id, title) VALUES ('3', 'World')", &[])
            .unwrap();
        let audited = conn
            .query_one("SELECT COUNT(*) AS c FROM audit WHERE item = '3'", &[])
            .unwrap()
            .unwrap()
            .get_i64("c")
            .unwrap();
        assert_eq!(audited, 1, "the custom trigger must survive the rebuild");
    }

    /// Regression: a view over the table, or a trigger on another table that
    /// writes to it, made the rename step fail ("error in view …: no such
    /// table") and stopped the schema sync at boot. They are set aside around
    /// the swap and recreated — a view over that view and an `INSTEAD OF`
    /// trigger on it too — and all keep working on the rebuilt table.
    #[test]
    fn rebuild_keeps_dependent_views_and_triggers_on_other_tables() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE items (id TEXT PRIMARY KEY, title TEXT NOT NULL, \
             _ref_count INTEGER NOT NULL DEFAULT 0, created_at TEXT, updated_at TEXT);
             CREATE TABLE inbox (title TEXT);
             CREATE VIEW item_titles AS SELECT id, title FROM main.items;
             CREATE VIEW loud_titles AS SELECT upper(title) AS title FROM item_titles;
             CREATE TRIGGER inbox_to_items AFTER INSERT ON inbox \
                 BEGIN INSERT INTO \"items\" (id, title) VALUES (NEW.title, NEW.title); END;
             CREATE TRIGGER item_titles_insert INSTEAD OF INSERT ON item_titles \
                 BEGIN INSERT INTO items (id, title) VALUES (NEW.id, NEW.title); END;
             INSERT INTO items (id, title) VALUES ('1', 'Hello');",
        )
        .unwrap();

        let def = simple_collection("items", vec![text_field("title")]);
        rebuild_table(&conn, "items", &def, &no_locale())
            .expect("dependents must not fail the rename");

        conn.execute("INSERT INTO inbox (title) VALUES ('Mail')", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO item_titles (id, title) VALUES ('2', 'Via view')",
            &[],
        )
        .unwrap();

        let rows = conn
            .query_all("SELECT title FROM loud_titles ORDER BY title", &[])
            .unwrap();
        let titles: Vec<String> = rows
            .iter()
            .map(|row| row.get_string("title").unwrap())
            .collect();

        assert_eq!(titles, ["HELLO", "MAIL", "VIA VIEW"]);
    }

    /// A rebuild outside the schema sync's foreign-key window would drop the
    /// old table with enforcement on, cascading into every child row. It is
    /// refused before anything changes.
    #[test]
    fn a_rebuild_with_foreign_keys_enforced_and_children_is_refused() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE items (id TEXT PRIMARY KEY, title TEXT NOT NULL, \
             _ref_count INTEGER NOT NULL DEFAULT 0, created_at TEXT, updated_at TEXT);
             CREATE TABLE items_tags (id TEXT PRIMARY KEY, \
             parent_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE);
             INSERT INTO items (id, title) VALUES ('1', 'Hello');
             INSERT INTO items_tags (id, parent_id) VALUES ('t1', '1');",
        )
        .unwrap();

        let def = simple_collection("items", vec![text_field("title")]);
        let err = rebuild_table(&conn, "items", &def, &no_locale())
            .unwrap_err()
            .to_string();
        assert!(err.contains("items_tags"), "{err}");

        let children = conn
            .query_one("SELECT COUNT(*) AS c FROM items_tags", &[])
            .unwrap()
            .unwrap()
            .get_i64("c")
            .unwrap();
        assert_eq!(children, 1, "the child row must be untouched");
    }

    /// The table an older release created: `title` required and the
    /// collection without drafts, so its column is `NOT NULL`.
    fn legacy_posts_table(conn: &dyn DbConnection) {
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT NOT NULL, \
             subtitle TEXT NOT NULL, _ref_count INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT, updated_at TEXT);
             INSERT INTO posts (id, title, subtitle) VALUES ('p1', 'Hello', 'Sub');",
        )
        .unwrap();
    }

    fn posts(required: bool) -> CollectionDefinition {
        simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("title", FieldType::Text)
                    .required(required)
                    .build(),
            ],
        )
    }

    /// Regression: the `NOT NULL` of a table created while `title` was
    /// required outlived the definition — with `required` removed, every write
    /// omitting the value failed at the database. The sync relaxes it once,
    /// keeping the rows, and a removed field's orphan column (`subtitle`)
    /// stops blocking creates too.
    #[test]
    fn sync_relaxes_not_null_of_an_older_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        legacy_posts_table(&conn);

        sync_collection_table(&conn, "posts", &posts(false), &no_locale()).unwrap();

        conn.execute("INSERT INTO posts (id) VALUES ('p2')", &[])
            .expect("neither the relaxed field nor the orphan column may block a create");

        let row = conn
            .query_one("SELECT title, subtitle FROM posts WHERE id = 'p1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("title").unwrap(), "Hello");
        assert_eq!(row.get_string("subtitle").unwrap(), "Sub");

        assert_eq!(
            columns_to_relax(&conn, "posts").unwrap(),
            None,
            "gate stamped"
        );
    }

    /// Enabling drafts on a collection whose table predates the change: a
    /// draft save skips `required`, so the column must accept NULL.
    #[test]
    fn sync_relaxes_not_null_when_drafts_are_enabled_later() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        legacy_posts_table(&conn);

        let mut def = posts(true);
        def.versions = Some(VersionsConfig::new(true, 10));
        sync_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        conn.execute(
            "INSERT INTO posts (id, _status) VALUES (?1, 'draft')",
            &[DbValue::Text("d1".into())],
        )
        .expect("a draft with the required field empty must be storable");
    }

    /// Through the whole schema sync, with a child table pointing at the
    /// collection: the rebuild runs inside the foreign-key window and the
    /// child rows keep their parent.
    #[test]
    fn schema_sync_relaxes_not_null_and_keeps_children() {
        let (_dir, pool) = in_memory_pool();

        {
            let conn = pool.get().unwrap();
            legacy_posts_table(&conn);
            conn.execute_batch(
                "CREATE TABLE _versions_posts (id TEXT PRIMARY KEY, \
                 _parent TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE, \
                 _version INTEGER NOT NULL, _status TEXT NOT NULL, \
                 _latest INTEGER NOT NULL DEFAULT 0, snapshot TEXT NOT NULL, \
                 created_at TEXT, updated_at TEXT);
                 INSERT INTO _versions_posts (id, _parent, _version, _status, snapshot) \
                 VALUES ('v1', 'p1', 1, 'published', '{}');",
            )
            .unwrap();
        }

        let mut def = posts(false);
        def.versions = Some(VersionsConfig::new(false, 10));
        let mut registry = Registry::new();
        registry.register_collection(def);

        sync_all(&pool, &registry, &no_locale()).expect("sync relaxes the old table");

        let conn = pool.get().unwrap();
        let versions = conn
            .query_one("SELECT COUNT(*) AS c FROM _versions_posts", &[])
            .unwrap()
            .unwrap()
            .get_i64("c")
            .unwrap();
        assert_eq!(versions, 1, "the version row must survive the rebuild");

        conn.execute("INSERT INTO posts (id) VALUES ('p2')", &[])
            .expect("the relaxed column accepts an omitted value");
    }

    /// A table the current release created has nothing to relax: the sync
    /// stamps the gate without rebuilding.
    #[test]
    fn a_fresh_table_is_stamped_without_a_rebuild() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        sync_collection_table(&conn, "posts", &posts(true), &no_locale()).unwrap();
        assert_eq!(
            columns_to_relax(&conn, "posts").unwrap(),
            Some(Vec::new()),
            "created without NOT NULL; gate stamped by the next sync"
        );

        sync_collection_table(&conn, "posts", &posts(true), &no_locale()).unwrap();
        assert_eq!(columns_to_relax(&conn, "posts").unwrap(), None);
    }

    /// A table an older release created for a collection whose `slug` was
    /// unique: the field carries an inline `UNIQUE`.
    fn legacy_unique_registry(unique: bool) -> Registry {
        let mut registry = Registry::new();
        registry.register_collection(simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(unique)
                    .build(),
            ],
        ));

        registry
    }

    fn legacy_unique_table(pool: &DbPool) {
        pool.get()
            .unwrap()
            .execute_batch(
                "CREATE TABLE posts (id TEXT PRIMARY KEY, slug TEXT UNIQUE, \
                 _ref_count INTEGER NOT NULL DEFAULT 0, created_at TEXT, updated_at TEXT); \
                 INSERT INTO posts (id, slug) VALUES ('p1', 'hello');",
            )
            .unwrap();
    }

    /// Regression: the inline `UNIQUE` an older release put on a unique field
    /// survived the upgrade, so removing `unique` from the field still failed
    /// every duplicate write at the database. The sync drops it once and keeps
    /// the rows.
    #[test]
    fn an_older_tables_inline_unique_is_dropped() {
        let (_dir, pool) = in_memory_pool();
        legacy_unique_table(&pool);

        sync_all(&pool, &legacy_unique_registry(false), &no_locale()).expect("sync");

        let conn = pool.get().unwrap();
        conn.execute("INSERT INTO posts (id, slug) VALUES ('p2', 'hello')", &[])
            .expect("a field without `unique` accepts a duplicate");
        assert!(
            conn.query_one("SELECT id FROM posts WHERE id = 'p1'", &[])
                .unwrap()
                .is_some(),
            "the old row is kept"
        );
    }

    /// A field still `unique` is enforced by its managed index once the inline
    /// constraint is gone.
    #[test]
    fn a_unique_field_keeps_its_uniqueness_through_the_drop() {
        let (_dir, pool) = in_memory_pool();
        legacy_unique_table(&pool);

        sync_all(&pool, &legacy_unique_registry(true), &no_locale()).expect("sync");

        let conn = pool.get().unwrap();
        conn.execute("INSERT INTO posts (id, slug) VALUES ('p2', 'hello')", &[])
            .expect_err("the managed unique index rejects the duplicate");

        let indexes = conn
            .query_all(
                "SELECT name FROM pragma_index_list('posts') WHERE origin = 'u'",
                &[],
            )
            .unwrap();
        assert!(indexes.is_empty(), "no inline UNIQUE is left");
    }
}
