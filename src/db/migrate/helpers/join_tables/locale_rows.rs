//! The rows of a join table whose field stopped keeping them per locale.
//!
//! A localized has-many relationship, array or blocks field stores one set of
//! rows per locale, each tagged with its `_locale`. When the field stops being
//! localized — `localized` cleared on it or on its group, or localization
//! turned off — its reads and writes no longer name a locale, so every
//! locale's rows would read back as one list: the default locale's entries
//! followed by every other locale's. The field's value is its default locale's
//! value from then on, the way a localized column's default-locale value is
//! carried into the plain column: the other locales' rows are dropped, once,
//! on the sync that sees the switch.
//!
//! Which tables kept rows per locale at the last sync is recorded under
//! `join_locale:{table}`, so a table is narrowed on the switch alone and never
//! again: rows written while the field is not localized take the `_locale`
//! column's default, which need not be today's default locale. A table synced
//! before the record existed that has a `_locale` column its field no longer
//! uses was switched by an earlier release, which kept every locale's rows;
//! it is narrowed on the first sync that records it.
//!
//! Dropped rows can hold references, so the reference counts are marked stale
//! and recomputed later in the same sync. A table holding rows of other
//! locales but none of the default locale is refused instead of emptied: that
//! is a default locale set to one the content never used, and narrowing would
//! drop every row.

use anyhow::{Context as _, Result, bail};
use tracing::info;

use crate::{
    config::LocaleConfig,
    db::{
        DbConnection,
        migrate::{
            backfill_ref_counts::invalidate_ref_counts,
            helpers::introspection::{get_table_columns, table_exists},
            meta,
        },
        query::{delete_rows_outside_locales, held_locales},
    },
};

/// Leads the recorded value; bump it to have every table recorded afresh.
const VERSION: &str = "1";

/// The `_crap_meta` key recording whether `table` kept rows per locale.
fn record_key(table: &str) -> String {
    format!("join_locale:{table}")
}

/// Whether `table` kept rows per locale at the last sync — `None` when that
/// was never recorded (or recorded under another version).
fn recorded(conn: &dyn DbConnection, table: &str) -> Result<Option<bool>> {
    let stored = meta::get(conn, &record_key(table))?;
    let shape = stored.as_deref().and_then(|v| v.strip_prefix(VERSION));

    Ok(match shape {
        Some(":per_locale") => Some(true),
        Some(":shared") => Some(false),
        _ => None,
    })
}

/// Record whether `table` keeps rows per locale now.
fn record(conn: &dyn DbConnection, table: &str, per_locale: bool) -> Result<()> {
    let shape = if per_locale { "per_locale" } else { "shared" };

    meta::upsert(conn, &record_key(table), &format!("{VERSION}:{shape}"))
}

/// Whether `table` still holds the rows of a field that kept them per locale:
/// it was recorded so, or — never recorded — it has the `_locale` column only
/// a localized field adds.
fn was_per_locale(conn: &dyn DbConnection, table: &str, recorded: Option<bool>) -> Result<bool> {
    if let Some(per_locale) = recorded {
        return Ok(per_locale);
    }

    if !table_exists(conn, table)? {
        return Ok(false);
    }

    Ok(get_table_columns(conn, table)?.contains("_locale"))
}

/// Keep only the default locale's rows of `table`, refusing when it holds
/// none of them. Returns how many rows were dropped.
fn keep_default_locale_rows(
    conn: &dyn DbConnection,
    table: &str,
    default_locale: &str,
) -> Result<usize> {
    let held = held_locales(conn, table)?;

    if held.iter().all(|locale| locale == default_locale) {
        return Ok(0);
    }

    if !held.iter().any(|locale| locale == default_locale) {
        bail!(
            "Join table '{table}' belongs to a field that is no longer localized, so it keeps \
             only its default locale's rows ('{default_locale}') — but it holds rows of {} \
             only. Set [locale] default_locale to the locale whose rows the field keeps, or \
             mark the field localized again, then start again.",
            held.join(", ")
        );
    }

    let dropped = delete_rows_outside_locales(conn, table, &[default_locale.to_string()])?;

    info!(
        "Dropped {dropped} row(s) of locales other than '{default_locale}' from '{table}': its \
         field is no longer localized"
    );

    Ok(dropped)
}

/// Bring `table`'s rows to what its field keeps now: when the field stopped
/// keeping rows per locale since the last sync, only the default locale's
/// rows stay. Records the field's shape for the next sync. Runs before the
/// table's DDL, which may re-key it by what the rows are unique under now.
///
/// # Errors
///
/// Returns an error when the table holds no row of the default locale but
/// rows of others, or a backend error if a read, the delete or a meta write
/// fails.
pub(super) fn sync_locale_rows(
    conn: &dyn DbConnection,
    table: &str,
    per_locale: bool,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let recorded = recorded(conn, table)?;

    if !per_locale && was_per_locale(conn, table, recorded)? {
        let dropped = keep_default_locale_rows(conn, table, &locale_config.default_locale)
            .with_context(|| format!("Failed to narrow '{table}' to its default locale"))?;

        if dropped > 0 {
            invalidate_ref_counts(conn)?;
        }
    }

    if recorded == Some(per_locale) {
        return Ok(());
    }

    record(conn, table, per_locale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{CollectionDefinition, FieldDefinition, FieldType, Registry, RelationshipConfig},
        db::{
            DbPool, DbValue,
            migrate::{
                collection::test_helpers::{in_memory_pool, locale_en_de, no_locale},
                sync_all,
            },
            query::find_related_ids,
        },
    };

    /// `posts` with a has-many `tags` relationship and an `items` array, both
    /// `localized` as given, beside the `tags` collection they reference.
    fn registry(localized: bool) -> Registry {
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .localized(localized)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .localized(localized)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                ])
                .build(),
        ];

        let mut registry = Registry::new();
        registry.register_collection(CollectionDefinition::new("tags"));
        registry.register_collection(posts);

        registry
    }

    /// A localized post whose `en` and `de` lists name different tags and
    /// hold different array rows.
    fn seeded_localized(pool: &DbPool) {
        sync_all(pool, &registry(true), &locale_en_de()).expect("localized sync");

        pool.get()
            .unwrap()
            .execute_batch(
                "INSERT INTO tags (id) VALUES ('t1'), ('t2'), ('t3');
                 INSERT INTO posts (id) VALUES ('p1');
                 INSERT INTO posts_tags (parent_id, related_id, _order, _locale) VALUES
                   ('p1', 't1', 0, 'en'), ('p1', 't2', 1, 'en'),
                   ('p1', 't2', 0, 'de'), ('p1', 't3', 1, 'de');
                 INSERT INTO posts_items (id, parent_id, _order, _locale, label) VALUES
                   ('i1', 'p1', 0, 'en', 'one'), ('i2', 'p1', 0, 'de', 'eins');
                 UPDATE tags SET _ref_count = 1 WHERE id IN ('t1', 't3');
                 UPDATE tags SET _ref_count = 2 WHERE id = 't2';",
            )
            .unwrap();
    }

    fn ref_count(pool: &DbPool, id: &str) -> i64 {
        pool.get()
            .unwrap()
            .query_one(
                "SELECT _ref_count FROM tags WHERE id = ?1",
                &[DbValue::Text(id.to_string())],
            )
            .unwrap()
            .and_then(|row| row.i64_at(0))
            .unwrap()
    }

    fn item_labels(pool: &DbPool) -> Vec<String> {
        pool.get()
            .unwrap()
            .query_all("SELECT label FROM posts_items ORDER BY _order, id", &[])
            .unwrap()
            .iter()
            .filter_map(|row| row.opt_text_at(0))
            .collect()
    }

    /// Regression: a has-many field that stopped being localized kept every
    /// locale's rows, and its reads — which no longer name a locale — returned
    /// them all as one list. The sync keeps the default locale's rows in their
    /// order, drops the others, and recounts the references they held.
    #[test]
    fn an_unlocalized_field_keeps_its_default_locales_rows() {
        let (_dir, pool) = in_memory_pool();
        seeded_localized(&pool);

        sync_all(&pool, &registry(false), &locale_en_de()).expect("unlocalized sync");

        let conn = pool.get().unwrap();
        assert_eq!(
            find_related_ids(&conn, "posts", "tags", "p1", None).unwrap(),
            vec!["t1", "t2"]
        );
        assert_eq!(item_labels(&pool), vec!["one"]);

        assert_eq!(ref_count(&pool, "t1"), 1);
        assert_eq!(ref_count(&pool, "t2"), 1, "the `de` reference is gone");
        assert_eq!(ref_count(&pool, "t3"), 0, "only `de` named it");
    }

    /// Turning localization off entirely narrows the same way.
    #[test]
    fn turning_localization_off_narrows_too() {
        let (_dir, pool) = in_memory_pool();
        seeded_localized(&pool);

        sync_all(&pool, &registry(true), &no_locale()).expect("sync without locales");

        let conn = pool.get().unwrap();
        assert_eq!(
            find_related_ids(&conn, "posts", "tags", "p1", None).unwrap(),
            vec!["t1", "t2"]
        );
    }

    /// Once narrowed, a later sync leaves the rows alone — also the ones a
    /// write stored under the column's default, which need not be today's
    /// default locale.
    #[test]
    fn a_table_is_narrowed_once() {
        let (_dir, pool) = in_memory_pool();
        seeded_localized(&pool);
        sync_all(&pool, &registry(false), &locale_en_de()).expect("unlocalized sync");

        pool.get()
            .unwrap()
            .execute(
                "INSERT INTO posts_items (id, parent_id, _order, _locale, label) \
                 VALUES ('i3', 'p1', 1, 'de', 'written since')",
                &[],
            )
            .unwrap();

        sync_all(&pool, &registry(false), &locale_en_de()).expect("second sync");

        assert_eq!(item_labels(&pool), vec!["one", "written since"]);
    }

    /// A table with rows of other locales but none of the default one is
    /// refused rather than emptied, and nothing is dropped.
    #[test]
    fn a_table_without_default_locale_rows_is_refused() {
        let (_dir, pool) = in_memory_pool();
        seeded_localized(&pool);
        pool.get()
            .unwrap()
            .execute("DELETE FROM posts_items WHERE _locale = 'en'", &[])
            .unwrap();

        let err =
            sync_all(&pool, &registry(false), &locale_en_de()).expect_err("no default-locale rows");
        let msg = format!("{err:#}");

        assert!(
            msg.contains("posts_items") && msg.contains("de only"),
            "{msg}"
        );
        assert_eq!(item_labels(&pool), vec!["eins"], "nothing is dropped");
    }
}
