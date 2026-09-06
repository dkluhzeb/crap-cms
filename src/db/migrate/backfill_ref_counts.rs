//! One-time backfill of `_ref_count` columns from existing relationship data.

use anyhow::{Context as _, Result};
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, Registry},
    db::{
        DbConnection,
        migrate::meta,
        query::{helpers::global_table, ref_count},
    },
};

/// Legacy "everything is backfilled" meta key.
const META_KEY: &str = "ref_count_backfilled";

/// Current backfill computation version. Stored as the meta *value* (not
/// baked into the key, which stays stable). Bump this whenever the ref-count
/// computation changes so existing databases recompute once on the next
/// startup. v2 added recursion into relationships nested in groups/arrays/
/// blocks and has-many relationships stored inside blocks.
const BACKFILL_VERSION: &str = "2";

/// Build the per-collection meta key for tracking backfill status.
fn collection_meta_key(slug: &str) -> String {
    format!("ref_count_backfilled:{slug}")
}

/// A collection/global is up to date only when its stored value matches the
/// current backfill version — a missing or stale value triggers a recompute.
fn is_backfilled(conn: &dyn DbConnection, slug: &str) -> Result<bool> {
    Ok(meta::get(conn, &collection_meta_key(slug))?.as_deref() == Some(BACKFILL_VERSION))
}

/// Mark a collection/global as backfilled at the current version.
fn mark_backfilled(conn: &dyn DbConnection, slug: &str) -> Result<()> {
    meta::upsert(conn, &collection_meta_key(slug), BACKFILL_VERSION)
}

/// Run the ref count backfill for any collections/globals not yet backfilled.
/// Must be called within a transaction after tables have been synced.
/// Tracks backfill status per-collection so newly added collections are covered.
pub(crate) fn backfill_if_needed(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    // Legacy "all backfilled" flag — only honored when it carries the current
    // version. An older value (or absence) means everything must recompute.
    let has_legacy_flag = meta::get(conn, META_KEY)?.as_deref() == Some(BACKFILL_VERSION);

    // Collect which collections/globals need backfilling. A DB error from
    // `is_backfilled` is PROPAGATED (`?`), not swallowed: the old
    // `.unwrap_or(true)` mapped a transient error to "already backfilled" and
    // silently dropped that collection from the backfill set, leaving its
    // `_ref_count` columns permanently wrong. Failing loud at startup migration
    // is correct — a broken DB should abort, not drift.
    let mut needs_backfill_collections = Vec::new();
    let mut needs_backfill_globals = Vec::new();

    if !has_legacy_flag {
        for (slug, def) in &registry.collections {
            if !is_backfilled(conn, slug)? {
                needs_backfill_collections.push((slug, def));
            }
        }

        for (slug, def) in &registry.globals {
            if !is_backfilled(conn, slug)? {
                needs_backfill_globals.push((slug, def));
            }
        }
    }

    if needs_backfill_collections.is_empty() && needs_backfill_globals.is_empty() {
        return Ok(());
    }

    info!("Backfilling _ref_count columns from existing relationship data...");

    // Phase 1: Reset ref counts to 0 for ALL collections (not just new ones),
    // because new collections may be referenced by existing ones.
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
    // ref-count walker, so every supported nesting depth is covered. We
    // re-walk everything because existing collections may reference newly
    // added ones.
    for (slug, def) in &registry.collections {
        recompute_table(conn, slug, &def.fields, locale_config)?;
    }

    for (slug, def) in &registry.globals {
        recompute_table(conn, &global_table(slug), &def.fields, locale_config)?;
    }

    // Phase 3: Mark newly backfilled collections.
    for (slug, _) in &needs_backfill_collections {
        mark_backfilled(conn, slug)?;
    }

    for (slug, _) in &needs_backfill_globals {
        mark_backfilled(conn, slug)?;
    }

    // Stamp the legacy flag at the current version so a fully up-to-date
    // database short-circuits on the next startup.
    if !has_legacy_flag {
        meta::upsert(conn, META_KEY, BACKFILL_VERSION)?;
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

        ref_count::after_create(conn, table, id, fields, locale_config)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrapConfig, DatabaseConfig};
    use crate::core::Slug;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::migrate::collection::test_helpers::no_locale;
    use crate::db::{DbConnection, DbPool, migrate, pool};

    fn setup_db(
        collections: &[CollectionDefinition],
        globals: &[GlobalDefinition],
        locale: &LocaleConfig,
    ) -> (tempfile::TempDir, DbPool, Registry) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..CrapConfig::test_default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

        let registry_shared = crate::core::Registry::shared();
        {
            let mut reg = registry_shared.write().unwrap();
            for c in collections {
                reg.register_collection(c.clone());
            }
            for g in globals {
                reg.register_global(g.clone());
            }
        }
        let registry = (*crate::core::Registry::snapshot(&registry_shared)).clone();
        migrate::sync_all(&db_pool, &registry, locale).expect("sync");

        (tmp, db_pool, registry)
    }

    fn get_ref_count(conn: &dyn DbConnection, table: &str, id: &str) -> i64 {
        crate::db::query::ref_count::get_ref_count(conn, table, id)
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

        // Simulate "pages was just added": clear the legacy flag and the pages
        // per-collection flag, but leave the posts per-collection flag intact.
        conn.execute(
            "DELETE FROM _crap_meta WHERE key = 'ref_count_backfilled'",
            &[],
        )
        .unwrap();
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
        assert!(
            is_backfilled(&conn, "posts").unwrap(),
            "posts per-collection flag should be set"
        );
        assert!(
            is_backfilled(&conn, "pages").unwrap(),
            "pages per-collection flag should be set"
        );
    }
}
