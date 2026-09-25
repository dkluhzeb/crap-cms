//! Ref-count delta computation and application.

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use anyhow::{Context as _, Result};
use tracing::{debug, trace, warn};

use crate::db::{DbConnection, DbValue, query::helpers::placeholder_list};

use super::outgoing_ref::OutgoingRef;

/// A write refused for referencing documents of `collection` it may not point
/// at: `ids` do not exist — or, for a live write, are in the trash. Both read
/// the same, so a writer learns nothing about a document it may not see.
/// Typed so the write layer can report it on the fields holding the
/// references (see [`anchor_to_fields`](super::anchor_to_fields)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnavailableReferences {
    pub collection: String,
    pub ids: Vec<String>,
}

impl fmt::Display for UnavailableReferences {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot reference {}/{}: no such document",
            self.collection,
            self.ids.join(", ")
        )
    }
}

impl Error for UnavailableReferences {}

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
/// Deltas are batched per (collection, `delta_value`) so that all targets
/// sharing the same collection and delta are updated in a single `UPDATE`
/// with an `IN` clause. This reduces round-trips from O(targets) to
/// O(distinct `collection×delta_sign` pairs) — typically 2-4 UPDATEs instead
/// of 5-8+ for a write touching multiple relationships.
///
/// Postgres takes a row-level write lock on each updated row implicitly
/// (READ COMMITTED default isolation), and `SQLite` serializes via the
/// `IMMEDIATE` transaction held by the caller.
pub(super) fn apply_deltas(
    conn: &dyn DbConnection,
    deltas: &HashMap<(String, String), i64>,
) -> Result<()> {
    apply_deltas_with(conn, deltas, MissingTarget::Reject)
}

/// What an increment against an unavailable target means to the caller.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MissingTarget {
    /// A live write: a NEW reference to a document that does not exist or is
    /// in the trash is refused, so the transaction rolls back. (A trashed
    /// document is hidden from every read, and a reference would pin it in
    /// the trash — a referenced document is never purged.)
    Reject,
    /// Stored references replayed onto a fresh copy of their documents (an
    /// import): a missing target is refused, but a trashed one is kept — the
    /// reference was stored before its target was trashed, as a document
    /// whose target is trashed later keeps its reference.
    RejectMissing,
    /// A repair replay over existing rows (the ref-count backfill): the
    /// dangling reference is already stored; skip it with a warning rather
    /// than refusing to start.
    Skip,
}

/// [`apply_deltas`] with an explicit missing-target policy.
pub(super) fn apply_deltas_with(
    conn: &dyn DbConnection,
    deltas: &HashMap<(String, String), i64>,
    on_missing: MissingTarget,
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
        // A new reference to a document a live write may not point at is
        // refused before anything counts it. The check locks the targets it
        // reads (Postgres), so none can move to the trash before the UPDATE.
        if *delta > 0 && on_missing == MissingTarget::Reject {
            refuse(collection, unavailable_ids(conn, collection, ids, true)?)?;
        }

        let in_clause = placeholder_list(conn, ids.len());

        let clamped = conn.greatest_expr("0", &format!("_ref_count + ({delta})"));
        let sql =
            format!("UPDATE \"{collection}\" SET _ref_count = {clamped} WHERE id IN ({in_clause})");

        let params: Vec<DbValue> = ids.iter().map(|id| DbValue::Text(id.to_string())).collect();

        let affected = conn.execute(&sql, &params).with_context(|| {
            format!("Failed to batch-update _ref_count on {collection} by {delta}")
        })?;

        // An increment against vanished targets: a write replaying stored
        // references refuses it, so the enclosing transaction rolls back
        // instead of storing dangling refs. A repair replay skips them (they
        // are already dangling; refusing would block startup on data it
        // cannot fix).
        if *delta > 0 && affected < ids.len() {
            let missing = unavailable_ids(conn, collection, ids, false)?;

            if on_missing != MissingTarget::Skip {
                refuse(collection, missing)?;
                continue;
            }

            warn!(
                "Ref-count backfill: {collection}/{} no longer exists — a dangling \
                 reference was skipped (clear or update the referencing document)",
                missing.join(", ")
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

/// Refuse the write when `unavailable` names any document.
fn refuse(collection: &str, unavailable: Vec<String>) -> Result<()> {
    if unavailable.is_empty() {
        return Ok(());
    }

    Err(UnavailableReferences {
        collection: collection.to_string(),
        ids: unavailable,
    }
    .into())
}

/// The ids among `ids` a reference may not point at: those with no row, and —
/// with `trashed_too` — those in the trash. On Postgres the rows read are
/// locked for the rest of the transaction.
///
/// Reads every column rather than naming `_deleted_at`, so it answers for a
/// collection without a trash column too.
fn unavailable_ids(
    conn: &dyn DbConnection,
    collection: &str,
    ids: &[&str],
    trashed_too: bool,
) -> Result<Vec<String>> {
    let in_clause = placeholder_list(conn, ids.len());
    let lock = if conn.is_postgres() {
        " FOR UPDATE"
    } else {
        ""
    };
    let sql = format!("SELECT * FROM \"{collection}\" WHERE id IN ({in_clause}){lock}");
    let params: Vec<DbValue> = ids.iter().map(|id| DbValue::Text(id.to_string())).collect();

    let rows = conn
        .query_all(&sql, &params)
        .with_context(|| format!("Failed to read reference targets in {collection}"))?;

    let available: HashSet<String> = rows
        .iter()
        .filter(|row| !trashed_too || row.get_named("_deleted_at").is_none_or(DbValue::is_null))
        .filter_map(|row| row.get_string("id").ok())
        .collect();

    Ok(ids
        .iter()
        .filter(|id| !available.contains(**id))
        .map(|id| (*id).to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CollectionDefinition;
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

    /// Regression: a write could reference a TRASHED document — the update
    /// matched the soft-deleted row — so every read then hid the target while
    /// the new reference pinned it in the trash; the success-or-failure answer
    /// also told a writer whether an id existed. A trashed target is refused
    /// with exactly the wording of a missing one.
    #[test]
    fn apply_deltas_refuses_a_trashed_target_like_a_missing_one() {
        let mut media = CollectionDefinition::new("media");
        media.soft_delete = true;
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m_trashed");
        insert_doc(&conn, "media", "m_live");
        conn.execute(
            "UPDATE media SET _deleted_at = '2026-01-01T00:00:00.000Z' WHERE id = 'm_trashed'",
            &[],
        )
        .unwrap();

        let refuse = |id: &str| {
            let deltas = HashMap::from([(("media".to_string(), id.to_string()), 1i64)]);
            let err = apply_deltas(&conn, &deltas).unwrap_err();
            let typed = err
                .downcast_ref::<UnavailableReferences>()
                .expect("a typed refusal");
            assert_eq!(typed.ids, vec![id.to_string()]);
            format!("{err:#}")
        };

        let trashed = refuse("m_trashed");
        let missing = refuse("m_missing");

        assert_eq!(
            trashed.replace("m_trashed", "ID"),
            missing.replace("m_missing", "ID"),
            "a trashed and a missing target read the same"
        );
        assert_eq!(get_ref_count_val(&conn, "media", "m_trashed"), 0);

        let live = HashMap::from([(("media".to_string(), "m_live".to_string()), 1i64)]);
        apply_deltas(&conn, &live).expect("a live target is referenced");
        assert_eq!(get_ref_count_val(&conn, "media", "m_live"), 1);
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

    /// Regression: an import replays the references its documents stored —
    /// including one whose target was trashed after it was referenced, which
    /// the export carries trashed. Refusing it (as a live write refuses a NEW
    /// reference to a trashed document) made such an export impossible to
    /// import. A missing target is still refused.
    #[test]
    fn replaying_stored_references_keeps_a_trashed_target() {
        let mut media = CollectionDefinition::new("media");
        media.soft_delete = true;
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m_trashed");
        conn.execute(
            "UPDATE media SET _deleted_at = '2026-01-01T00:00:00.000Z' WHERE id = 'm_trashed'",
            &[],
        )
        .unwrap();

        let replay = |id: &str| {
            let deltas = HashMap::from([(("media".to_string(), id.to_string()), 1i64)]);
            apply_deltas_with(&conn, &deltas, MissingTarget::RejectMissing)
        };

        replay("m_trashed").expect("a stored reference to a trashed target is kept");
        assert_eq!(get_ref_count_val(&conn, "media", "m_trashed"), 1);

        let err = replay("m_missing").expect_err("a missing target is refused");
        assert!(err.downcast_ref::<UnavailableReferences>().is_some());
    }
}
