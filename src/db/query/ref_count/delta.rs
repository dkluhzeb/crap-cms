//! Ref-count delta computation and application.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result, bail};
use tracing::{debug, trace};

use crate::db::{DbConnection, DbValue};

use super::outgoing_ref::OutgoingRef;

/// Compute ref count deltas between old and new outgoing ref sets.
pub(super) fn to_delta_map(
    old_refs: &[OutgoingRef],
    new_refs: &[OutgoingRef],
) -> HashMap<(String, String), i64> {
    let mut deltas: HashMap<(String, String), i64> = HashMap::new();

    for r in old_refs {
        *deltas
            .entry((r.target_collection.clone(), r.target_id.clone()))
            .or_insert(0) -= 1;
    }

    for r in new_refs {
        *deltas
            .entry((r.target_collection.clone(), r.target_id.clone()))
            .or_insert(0) += 1;
    }

    // Remove zero-deltas
    deltas.retain(|_, v| *v != 0);

    deltas
}

/// Apply ref count deltas to target collection tables.
///
/// Deltas are batched per (collection, delta_value) so that all targets
/// sharing the same collection and delta are updated in a single `UPDATE`
/// with an `IN` clause. This reduces round-trips from O(targets) to
/// O(distinct collection×delta_sign pairs) — typically 2-4 UPDATEs instead
/// of 5-8+ for a write touching multiple relationships.
///
/// Postgres takes a row-level write lock on each updated row implicitly
/// (READ COMMITTED default isolation), and SQLite serializes via the
/// `IMMEDIATE` transaction held by the caller.
pub(super) fn apply_deltas(
    conn: &dyn DbConnection,
    deltas: &HashMap<(String, String), i64>,
) -> Result<()> {
    if deltas.is_empty() {
        return Ok(());
    }

    // Group by (collection, delta_value) → Vec<id>
    let mut groups: HashMap<(&str, i64), Vec<&str>> = HashMap::new();

    for ((collection, id), delta) in deltas {
        groups
            .entry((collection.as_str(), *delta))
            .or_default()
            .push(id.as_str());
    }

    for ((collection, delta), ids) in &groups {
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| conn.placeholder(i)).collect();
        let in_clause = placeholders.join(", ");

        let clamped = conn.greatest_expr("0", &format!("_ref_count + ({})", delta));
        let sql =
            format!("UPDATE \"{collection}\" SET _ref_count = {clamped} WHERE id IN ({in_clause})");

        let params: Vec<DbValue> = ids.iter().map(|id| DbValue::Text(id.to_string())).collect();

        let affected = conn.execute(&sql, &params).with_context(|| {
            format!(
                "Failed to batch-update _ref_count on {} by {}",
                collection, delta
            )
        })?;

        // Increment against vanished targets is a hard error: the caller is
        // about to persist references to rows that no longer exist. Bail so
        // the enclosing transaction rolls back, preventing dangling refs.
        if *delta > 0 && affected < ids.len() {
            let missing = find_missing_ids(conn, collection, ids);
            bail!(
                "cannot reference {}/{}: target no longer exists \
                 (concurrently hard-deleted)",
                collection,
                missing
            );
        }

        // Decrement against missing targets is tolerated: soft-delete never
        // decrements, so a missing row means a concurrent hard-delete already
        // removed it. Nothing left to adjust.
        if *delta < 0 && affected < ids.len() {
            let skipped = ids.len() - affected;
            debug!("Skipped decrement on {skipped} target(s) in {collection}: already gone");
        }

        if *delta < 0 {
            trace!(
                "Decremented _ref_count on {} target(s) in {collection} by {}",
                affected,
                delta.abs()
            );
        }
    }

    Ok(())
}

/// Find which ids from a batch are missing from the table. Used only on
/// the error path to produce a specific error message.
fn find_missing_ids(conn: &dyn DbConnection, collection: &str, ids: &[&str]) -> String {
    let placeholders: Vec<String> = (1..=ids.len()).map(|i| conn.placeholder(i)).collect();
    let in_clause = placeholders.join(", ");
    let sql = format!("SELECT id FROM \"{collection}\" WHERE id IN ({in_clause})");

    let params: Vec<DbValue> = ids.iter().map(|id| DbValue::Text(id.to_string())).collect();

    let Ok(rows) = conn.query_all(&sql, &params) else {
        return ids.join(", ");
    };

    let found: HashSet<String> = rows
        .iter()
        .filter_map(|r| r.get_string("id").ok())
        .collect();

    ids.iter()
        .filter(|id| !found.contains(**id))
        .copied()
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::CollectionDefinition;
    use crate::db::query::ref_count::outgoing_ref::OutgoingRef;
    use crate::db::query::ref_count::test_helpers::*;

    // ── to_delta_map ─────────────────────────────────────────────────────

    #[test]
    fn delta_map_add_refs() {
        let new = vec![
            OutgoingRef {
                target_collection: "media".into(),
                target_id: "m1".into(),
            },
            OutgoingRef {
                target_collection: "media".into(),
                target_id: "m2".into(),
            },
        ];
        let deltas = to_delta_map(&[], &new);
        assert_eq!(deltas.get(&("media".into(), "m1".into())), Some(&1));
        assert_eq!(deltas.get(&("media".into(), "m2".into())), Some(&1));
    }

    #[test]
    fn delta_map_remove_refs() {
        let old = vec![OutgoingRef {
            target_collection: "media".into(),
            target_id: "m1".into(),
        }];
        let deltas = to_delta_map(&old, &[]);
        assert_eq!(deltas.get(&("media".into(), "m1".into())), Some(&-1));
    }

    #[test]
    fn delta_map_swap_refs() {
        let old = vec![OutgoingRef {
            target_collection: "media".into(),
            target_id: "m1".into(),
        }];
        let new = vec![OutgoingRef {
            target_collection: "media".into(),
            target_id: "m2".into(),
        }];
        let deltas = to_delta_map(&old, &new);
        assert_eq!(deltas.get(&("media".into(), "m1".into())), Some(&-1));
        assert_eq!(deltas.get(&("media".into(), "m2".into())), Some(&1));
    }

    #[test]
    fn delta_map_no_change() {
        let refs = vec![OutgoingRef {
            target_collection: "media".into(),
            target_id: "m1".into(),
        }];
        let deltas = to_delta_map(&refs, &refs);
        assert!(deltas.is_empty());
    }

    #[test]
    fn delta_map_duplicate_refs() {
        let old = vec![
            OutgoingRef {
                target_collection: "media".into(),
                target_id: "m1".into(),
            },
            OutgoingRef {
                target_collection: "media".into(),
                target_id: "m1".into(),
            },
        ];
        let new = vec![OutgoingRef {
            target_collection: "media".into(),
            target_id: "m1".into(),
        }];
        let deltas = to_delta_map(&old, &new);
        assert_eq!(deltas.get(&("media".into(), "m1".into())), Some(&-1));
    }

    // ── apply_deltas ─────────────────────────────────────────────────────

    #[test]
    fn apply_deltas_mixed_inc_dec() {
        let media = CollectionDefinition::new("media");
        let tags = CollectionDefinition::new("tags");
        let (_tmp, pool, _) = setup_db(&[media, tags], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "tags", "t1");

        // Set initial ref counts
        conn.execute("UPDATE media SET _ref_count = 3 WHERE id = 'm1'", &[])
            .unwrap();
        conn.execute("UPDATE tags SET _ref_count = 0 WHERE id = 't1'", &[])
            .unwrap();

        let mut deltas = HashMap::new();
        deltas.insert(("media".to_string(), "m1".to_string()), -2i64);
        deltas.insert(("tags".to_string(), "t1".to_string()), 1i64);

        apply_deltas(&conn, &deltas).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
        assert_eq!(get_ref_count_val(&conn, "tags", "t1"), 1);
    }

    /// Regression: `apply_deltas` must fail loudly when an increment targets
    /// a row that no longer exists. Previously this was silently logged as an
    /// error, leaving the caller with a dangling reference.
    #[test]
    fn apply_deltas_increment_on_missing_target_fails() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        // No row inserted for "m_missing" — target does not exist.
        let mut deltas = HashMap::new();
        deltas.insert(("media".to_string(), "m_missing".to_string()), 1i64);

        let err = apply_deltas(&conn, &deltas).expect_err("increment on missing target must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("media") && msg.contains("m_missing"),
            "error should mention the missing target, got: {msg}"
        );
    }

    /// Decrement against a missing target is a tolerated no-op — the target
    /// is gone so there's nothing to adjust. Only hard-delete decrements, and
    /// a concurrent hard-delete already removed the row.
    #[test]
    fn apply_deltas_decrement_on_missing_target_is_noop() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        let mut deltas = HashMap::new();
        deltas.insert(("media".to_string(), "m_missing".to_string()), -1i64);

        apply_deltas(&conn, &deltas).expect("decrement on missing target should be a no-op");
    }

    /// Happy path: increment against an existing target succeeds and updates
    /// the `_ref_count`. Guards against regressing the normal flow while
    /// adding the dangling-reference check.
    #[test]
    fn apply_deltas_increment_succeeds_when_target_exists() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");

        let mut deltas = HashMap::new();
        deltas.insert(("media".to_string(), "m1".to_string()), 2i64);

        apply_deltas(&conn, &deltas).expect("increment on existing target should succeed");

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 2);
    }

    /// When a batch of deltas contains a mix of valid targets and one missing
    /// target on an increment, the whole call must fail — callers rely on the
    /// transaction rolling back to avoid partial writes.
    #[test]
    fn apply_deltas_batched_increment_fails_if_any_target_missing() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");

        let mut deltas = HashMap::new();
        deltas.insert(("media".to_string(), "m1".to_string()), 1i64);
        deltas.insert(("media".to_string(), "m_missing".to_string()), 1i64);

        apply_deltas(&conn, &deltas).expect_err("batch must fail if any increment target missing");
    }
}
