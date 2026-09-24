//! Running the ref-count backfill: deciding at startup whether the stored
//! counts are current, and recomputing every count from the stored references
//! when they are not.

use anyhow::{Context as _, Result};
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, Registry},
    db::{
        DbConnection,
        migrate::{backfill_ref_counts::topology::gate_value, meta},
        query::{helpers::global_table, ref_count},
    },
};

/// Build the per-collection meta key for tracking backfill status.
fn collection_meta_key(slug: &str) -> String {
    format!("ref_count_backfilled:{slug}")
}

/// Every gated slug with the table its documents live in.
fn gated_tables(registry: &Registry) -> Vec<(&str, String)> {
    let collections = registry
        .collections
        .keys()
        .map(|slug| (&**slug, slug.to_string()));
    let globals = registry
        .globals
        .keys()
        .map(|slug| (&**slug, global_table(slug)));

    collections.chain(globals).collect()
}

/// Whether `table` holds no row at all.
fn table_is_empty(conn: &dyn DbConnection, table: &str) -> Result<bool> {
    let row = conn
        .query_one(&format!("SELECT 1 FROM \"{table}\" LIMIT 1"), &[])
        .with_context(|| format!("Backfill: failed to probe {table}"))?;

    Ok(row.is_none())
}

/// What the startup check must do for one slug.
enum GateState {
    /// Stamped at the current gate: nothing to do.
    Current,
    /// Never stamped, and its table holds no row: no count can be on it and
    /// none of its references can count anywhere, so stamping it is enough.
    NewAndEmpty,
    /// Stamped at another gate, or never stamped with rows already stored:
    /// every count must be recomputed.
    Stale,
}

/// Classify one slug against the current gate.
fn gate_state(conn: &dyn DbConnection, slug: &str, table: &str, gate: &str) -> Result<GateState> {
    match meta::get(conn, &collection_meta_key(slug))? {
        Some(stored) if stored == gate => Ok(GateState::Current),
        None if table_is_empty(conn, table)? => Ok(GateState::NewAndEmpty),
        _ => Ok(GateState::Stale),
    }
}

/// Run the ref count backfill when any collection/global is not backfilled at
/// the current gate.
/// Must be called within a transaction after tables have been synced.
/// Tracks backfill status per-collection so newly added collections are covered.
///
/// A newly added collection or global whose table is still empty is stamped
/// without a recount: it holds nothing to count, and nothing can count on it
/// yet. One whose table already holds rows (re-added after being removed, or
/// a database from before the gate existed) recomputes everything once.
pub(crate) fn backfill_if_needed(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    // Detect staleness ALWAYS by the per-slug value — a collection added after
    // the initial backfill must be covered even if the database was already
    // fully backfilled once. (The old global `ref_count_backfilled` flag
    // short-circuited this once set, so a later-added collection was silently
    // never backfilled — an under-count that defeats O(1) delete protection.)
    // The per-slug reads are cheap, and a fully up-to-date database returns
    // without writing.
    //
    // A DB error from the per-slug check is PROPAGATED (`?`), not swallowed:
    // mapping a transient error to "already backfilled" would silently drop
    // that collection from the backfill set, leaving its `_ref_count` columns
    // permanently wrong. Failing loud at startup migration is correct — a
    // broken DB should abort, not drift.
    let gate = gate_value(registry, locale_config);
    let mut new_and_empty = Vec::new();

    for (slug, table) in gated_tables(registry) {
        match gate_state(conn, slug, &table, &gate)? {
            GateState::Current => {}
            GateState::NewAndEmpty => new_and_empty.push(slug),
            GateState::Stale => return recompute_ref_counts(conn, registry, locale_config),
        }
    }

    for slug in new_and_empty {
        meta::upsert(conn, &collection_meta_key(slug), &gate)?;
    }

    Ok(())
}

/// Recompute every `_ref_count` from the stored references and stamp every
/// collection's and global's gate. Must run inside a transaction.
///
/// The recompute is always whole-database: a count on one collection is made
/// of every other collection's references, so a new or changed collection can
/// change counts anywhere. Besides the startup backfill, a maintenance write
/// that removes stored references (e.g. `db cleanup` dropping stale-locale
/// rows) calls it in its own transaction so the counts never outlive the rows.
///
/// # Errors
///
/// Returns an error when a count can't be reset, read or written, or a gate
/// can't be stamped.
pub(crate) fn recompute_ref_counts(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    info!("Backfilling _ref_count columns from existing relationship data...");

    // Phase 1: Reset ref counts to 0 for ALL collections, because any
    // collection may be referenced by any other.
    for slug in registry.collections.keys() {
        conn.execute(&format!("UPDATE \"{slug}\" SET _ref_count = 0"), &[])?;
    }

    for slug in registry.globals.keys() {
        conn.execute(
            &format!("UPDATE \"{}\" SET _ref_count = 0", global_table(slug)),
            &[],
        )?;
    }

    // Phase 2: Recompute outgoing refs per document via the canonical
    // ref-count walker, so every supported nesting depth is covered.
    for (slug, def) in &registry.collections {
        recompute_table(conn, slug, &def.fields, locale_config)?;
    }

    for (slug, def) in &registry.globals {
        recompute_table(conn, &global_table(slug), &def.fields, locale_config)?;
    }

    // Phase 3: Stamp every gate — all counts are now current.
    let gate = gate_value(registry, locale_config);

    for slug in registry.collections.keys().chain(registry.globals.keys()) {
        meta::upsert(conn, &collection_meta_key(slug), &gate)?;
    }

    info!("Ref count backfill complete");

    Ok(())
}

/// Recompute outgoing refs for every row in a table by replaying the
/// canonical `after_create` walk (a pure increment over the unified
/// ref-count walker) for each document. Counts must already be reset to 0,
/// so replaying every document yields correct absolute totals — and inherits
/// full recursion into groups/arrays/blocks for free.
fn recompute_table(
    conn: &dyn DbConnection,
    table: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    // Propagate a read failure rather than skipping the table: Phase 1 already
    // reset every `_ref_count` to 0, so a silently-skipped table would keep the
    // wrong (zeroed) counts while the caller still stamps the backfill gate.
    // Aborting leaves the gate unset, so the backfill re-runs on the next start.
    let rows = conn
        .query_all(&format!("SELECT id FROM \"{table}\""), &[])
        .with_context(|| format!("Backfill: failed to read ids from {table}"))?;

    for row in &rows {
        let Some(id) = row.text_at(0) else {
            continue;
        };

        ref_count::backfill_after_create(conn, table, id, fields, locale_config)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::{
        config::{CrapConfig, DatabaseConfig},
        core::{CollectionDefinition, FieldType, GlobalDefinition, RelationshipConfig, Slug},
        db::{
            DbPool,
            migrate::{
                self,
                backfill_ref_counts::{
                    test_support::{posts_with, registry_of, upload_to},
                    topology::BACKFILL_VERSION,
                },
                collection::test_helpers::no_locale,
            },
            pool,
        },
    };

    fn setup_db(
        collections: &[CollectionDefinition],
        globals: &[GlobalDefinition],
        locale: &LocaleConfig,
    ) -> (TempDir, DbPool, Registry) {
        let tmp = tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..CrapConfig::test_default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

        let registry_shared = Registry::shared();
        {
            let mut reg = registry_shared.write().unwrap();
            for c in collections {
                reg.register_collection(c.clone());
            }
            for g in globals {
                reg.register_global(g.clone());
            }
        }
        let registry = (*Registry::snapshot(&registry_shared)).clone();
        migrate::sync_all(&db_pool, &registry, locale).expect("sync");

        (tmp, db_pool, registry)
    }

    /// Store `gate` as `slug`'s backfill gate.
    fn stamp(conn: &dyn DbConnection, slug: &str, gate: &str) {
        meta::upsert(conn, &collection_meta_key(slug), gate).unwrap();
    }

    /// Whether `slug` is stamped at `gate`.
    fn is_backfilled(conn: &dyn DbConnection, slug: &str, gate: &str) -> Result<bool> {
        let stored = meta::get(conn, &collection_meta_key(slug))?;

        Ok(stored.as_deref() == Some(gate))
    }

    fn get_ref_count(conn: &dyn DbConnection, table: &str, id: &str) -> i64 {
        ref_count::get_ref_count(conn, table, id)
            .unwrap()
            .expect("document should exist")
    }

    // ── Basic backfill ───────────────────────────────────────────────────

    #[test]
    fn backfill_has_one_relationships() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        // Insert data bypassing ref counting
        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO media (id) VALUES ('m2')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p1', 'm1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p2', 'm1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p3', 'm2')", &[])
            .unwrap();

        // Clear the backfill flag so it runs again
        conn.execute(
            "DELETE FROM _crap_meta WHERE key LIKE 'ref_count_backfilled%'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        assert_eq!(get_ref_count(&conn, "media", "m1"), 2);
        assert_eq!(get_ref_count(&conn, "media", "m2"), 1);
    }

    /// A stored reference whose target no longer exists (a crash between a
    /// hard delete and its ref-count update, or direct SQL) must not abort
    /// the backfill — and with it startup. It is skipped; intact references
    /// still count.
    #[test]
    fn backfill_skips_a_dangling_reference() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p1', 'm1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p2', 'ghost')", &[])
            .unwrap();
        conn.execute(
            "DELETE FROM _crap_meta WHERE key LIKE 'ref_count_backfilled%'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale())
            .expect("a dangling reference is skipped, not fatal");

        assert_eq!(get_ref_count(&conn, "media", "m1"), 1);
    }

    // ── Has-many backfill ────────────────────────────────────────────────

    #[test]
    fn backfill_has_many_relationships() {
        let tags = CollectionDefinition::new("tags");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[tags, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO tags (id) VALUES ('t1')", &[])
            .unwrap();
        conn.execute("INSERT INTO tags (id) VALUES ('t2')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id) VALUES ('p2')", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES ('p1', 't1', 0)",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES ('p1', 't2', 1)",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES ('p2', 't1', 0)",
            &[],
        )
        .unwrap();

        conn.execute(
            "DELETE FROM _crap_meta WHERE key LIKE 'ref_count_backfilled%'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        assert_eq!(get_ref_count(&conn, "tags", "t1"), 2);
        assert_eq!(get_ref_count(&conn, "tags", "t2"), 1);
    }

    // ── Idempotent (second run is no-op) ─────────────────────────────────

    #[test]
    fn backfill_is_idempotent() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p1', 'm1')", &[])
            .unwrap();

        conn.execute(
            "DELETE FROM _crap_meta WHERE key LIKE 'ref_count_backfilled%'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();
        assert_eq!(get_ref_count(&conn, "media", "m1"), 1);

        // Second run should be a no-op (flag is set)
        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();
        assert_eq!(
            get_ref_count(&conn, "media", "m1"),
            1,
            "Should not double-count"
        );
    }

    // ── Polymorphic has-many ─────────────────────────────────────────────

    #[test]
    fn backfill_polymorphic_has_many() {
        let media = CollectionDefinition::new("media");
        let pages = CollectionDefinition::new("pages");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("related", FieldType::Relationship)
                .relationship(RelationshipConfig {
                    collection: Slug::new("media"),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec![Slug::new("media"), Slug::new("pages")],
                })
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, pages, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO pages (id) VALUES ('pg1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES ('p1', 'm1', 'media', 0)",
            &[],
        ).unwrap();
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES ('p1', 'pg1', 'pages', 1)",
            &[],
        ).unwrap();

        conn.execute(
            "DELETE FROM _crap_meta WHERE key LIKE 'ref_count_backfilled%'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        assert_eq!(get_ref_count(&conn, "media", "m1"), 1);
        assert_eq!(get_ref_count(&conn, "pages", "pg1"), 1);
    }

    // ── Global outgoing refs ─────────────────────────────────────────────

    #[test]
    fn backfill_global_outgoing_refs() {
        let media = CollectionDefinition::new("media");
        let mut settings = GlobalDefinition::new("settings");
        settings.fields = vec![
            FieldDefinition::builder("logo", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media], &[settings], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("UPDATE _global_settings SET logo = 'm1'", &[])
            .unwrap();

        conn.execute(
            "DELETE FROM _crap_meta WHERE key LIKE 'ref_count_backfilled%'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        assert_eq!(get_ref_count(&conn, "media", "m1"), 1);
    }

    // ── Array sub-field refs ─────────────────────────────────────────────

    #[test]
    fn backfill_array_sub_field_refs() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("slides", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("image", FieldType::Upload)
                        .relationship(RelationshipConfig::new("media", false))
                        .build(),
                ])
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO posts_slides (id, parent_id, _order, image) VALUES ('s1', 'p1', 0, 'm1')",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_slides (id, parent_id, _order, image) VALUES ('s2', 'p1', 1, 'm1')",
            &[],
        )
        .unwrap();

        conn.execute(
            "DELETE FROM _crap_meta WHERE key LIKE 'ref_count_backfilled%'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        assert_eq!(get_ref_count(&conn, "media", "m1"), 2);
    }

    // ── New collection after initial backfill ───────────────────────────

    #[test]
    fn backfill_new_collection_after_initial() {
        let media = CollectionDefinition::new("media");

        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let mut pages = CollectionDefinition::new("pages");
        pages.fields = vec![
            FieldDefinition::builder("hero", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        // Initial setup creates all tables and runs backfill (sets legacy + per-collection flags).
        let (_tmp, pool, registry) = setup_db(&[media, posts, pages], &[], &no_locale());
        let conn = pool.get().unwrap();

        // Insert media documents (bypassing ref counting).
        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO media (id) VALUES ('m2')", &[])
            .unwrap();

        // Posts referencing media (bypassing ref counting).
        conn.execute("INSERT INTO posts (id, image) VALUES ('p1', 'm1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p2', 'm1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p3', 'm2')", &[])
            .unwrap();

        // Pages referencing media (bypassing ref counting).
        conn.execute("INSERT INTO pages (id, hero) VALUES ('pg1', 'm1')", &[])
            .unwrap();
        conn.execute("INSERT INTO pages (id, hero) VALUES ('pg2', 'm2')", &[])
            .unwrap();

        // Simulate a database upgraded from the era that wrote a global
        // `ref_count_backfilled` flag, with `pages` added AFTER that initial
        // backfill: set the (now-ignored) legacy flag and drop only the pages
        // per-collection flag, leaving posts' intact. The backfill must still
        // detect and cover pages — a stale legacy flag must not short-circuit
        // per-collection detection (the bug this guards against).
        meta::upsert(&conn, "ref_count_backfilled", BACKFILL_VERSION).unwrap();
        conn.execute(
            "DELETE FROM _crap_meta WHERE key = 'ref_count_backfilled:pages'",
            &[],
        )
        .unwrap();

        // Run backfill again — should detect pages as new, do a full re-walk of
        // ALL collections (resetting counts to 0 first), and recompute everything.
        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        // m1 is referenced by p1, p2 (posts) + pg1 (pages) = 3
        assert_eq!(get_ref_count(&conn, "media", "m1"), 3);
        // m2 is referenced by p3 (posts) + pg2 (pages) = 2
        assert_eq!(get_ref_count(&conn, "media", "m2"), 2);

        // Per-collection flags should be set for both.
        let gate = gate_value(&registry, &no_locale());
        assert!(
            is_backfilled(&conn, "posts", &gate).unwrap(),
            "posts per-collection flag should be set"
        );
        assert!(
            is_backfilled(&conn, "pages", &gate).unwrap(),
            "pages per-collection flag should be set"
        );
    }

    /// Removing (or adding) a locale changes which columns the count walks, so
    /// a gate stamped under the old locale list must not pass. It used to, and
    /// the dropped locale's references stayed counted forever — phantom counts
    /// that block deletes.
    #[test]
    fn a_changed_locale_list_reopens_the_backfill_gate() {
        let en_de = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let en_only = LocaleConfig {
            locales: vec!["en".to_string()],
            ..en_de.clone()
        };

        let media = CollectionDefinition::new("media");
        let (_tmp, pool, registry) = setup_db(&[media], &[], &en_de);
        let conn = pool.get().unwrap();

        let gate_en_de = gate_value(&registry, &en_de);
        let gate_en = gate_value(&registry, &en_only);

        stamp(&conn, "media", &gate_en_de);

        assert!(is_backfilled(&conn, "media", &gate_en_de).unwrap());
        assert!(
            !is_backfilled(&conn, "media", &gate_en).unwrap(),
            "dropping a locale must force a recompute"
        );

        stamp(&conn, "media", &gate_en);
        assert!(is_backfilled(&conn, "media", &gate_en).unwrap());
    }

    /// Listing the same locales in another order changes no column the walk
    /// visits, so the gate must stay closed: reopening it re-counted every
    /// reference of every collection on the next boot for nothing.
    #[test]
    fn reordering_the_locales_keeps_the_backfill_gate_closed() {
        let en_de = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let reordered = LocaleConfig {
            locales: vec!["de".to_string(), "en".to_string()],
            ..en_de.clone()
        };

        let media = CollectionDefinition::new("media");
        let (_tmp, pool, registry) = setup_db(&[media], &[], &en_de);
        let conn = pool.get().unwrap();

        stamp(&conn, "media", &gate_value(&registry, &en_de));

        assert!(
            is_backfilled(&conn, "media", &gate_value(&registry, &reordered)).unwrap(),
            "a reordered locale list must not reopen the gate"
        );
    }

    /// Regression: removing a relationship/upload field left its references
    /// counted forever — the gate only knew the version and the locales, so
    /// the backfill never re-ran and the target stayed undeletable.
    #[test]
    fn removing_a_reference_field_recomputes_the_counts() {
        let media = CollectionDefinition::new("media");
        let posts = posts_with(vec![upload_to("image", "media")]);

        let (_tmp, pool, registry) = setup_db(&[media.clone(), posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p1', 'm1')", &[])
            .unwrap();
        recompute_ref_counts(&conn, &registry, &no_locale()).unwrap();
        assert_eq!(get_ref_count(&conn, "media", "m1"), 1);

        // The field is gone from the schema; its column (and value) stays.
        let without = registry_of(&[media, posts_with(Vec::new())]);
        backfill_if_needed(&conn, &without, &no_locale()).unwrap();

        assert_eq!(
            get_ref_count(&conn, "media", "m1"),
            0,
            "a removed field's references must stop counting"
        );
    }

    /// Mark `m1`'s count with a value no recount would produce, so a test can
    /// tell whether the backfill recounted.
    fn mark_count(conn: &dyn DbConnection) {
        conn.execute("UPDATE media SET _ref_count = 99 WHERE id = 'm1'", &[])
            .unwrap();
    }

    /// A newly added collection whose table is still empty is stamped without
    /// a recount; one whose table already holds rows (re-added after removal)
    /// recounts everything once.
    #[test]
    fn a_new_empty_collection_is_stamped_without_a_recount() {
        let media = CollectionDefinition::new("media");
        let posts = posts_with(vec![upload_to("image", "media")]);
        let notes = CollectionDefinition::new("notes");

        let (_tmp, pool, registry) = setup_db(&[media, posts, notes], &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p1', 'm1')", &[])
            .unwrap();
        recompute_ref_counts(&conn, &registry, &no_locale()).unwrap();

        // `notes` is new: never stamped, no rows.
        conn.execute(
            "DELETE FROM _crap_meta WHERE key = 'ref_count_backfilled:notes'",
            &[],
        )
        .unwrap();
        mark_count(&conn);

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        let gate = gate_value(&registry, &no_locale());
        assert!(is_backfilled(&conn, "notes", &gate).unwrap());
        assert_eq!(
            get_ref_count(&conn, "media", "m1"),
            99,
            "a new, empty collection must not trigger a recount"
        );

        // `notes` re-added with rows it kept from before: recount.
        conn.execute(
            "DELETE FROM _crap_meta WHERE key = 'ref_count_backfilled:notes'",
            &[],
        )
        .unwrap();
        conn.execute("INSERT INTO notes (id) VALUES ('n1')", &[])
            .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        assert_eq!(
            get_ref_count(&conn, "media", "m1"),
            1,
            "an unstamped table with rows must recount"
        );
        assert!(is_backfilled(&conn, "notes", &gate).unwrap());
    }
}
