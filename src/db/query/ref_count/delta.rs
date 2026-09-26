//! Ref-count delta computation and application.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    fmt,
};

use anyhow::{Context as _, Result};
use tracing::{debug, trace, warn};

use crate::db::{DbConnection, DbValue, query::helpers::placeholder_list};

use super::outgoing_ref::OutgoingRef;

/// A write refused for referencing documents of `collection` it may not point
/// at: `ids` do not exist — or, for a live write, are in the trash, or are new
/// references to documents the writer may not read (judged by the service
/// layer, which holds the writer's access rules). All read the same, so a
/// writer learns nothing about a document it may not see.
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
/// See [`apply_deltas_with`] for the locking order every call follows.
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

/// Every target of one write, per collection, both levels sorted: the one
/// order in which every writer takes its target row locks.
type SortedDeltas<'a> = BTreeMap<&'a str, BTreeMap<&'a str, i64>>;

/// What the lock pass found among one collection's targets.
#[derive(Default)]
struct LockedTargets {
    /// Ids with a row.
    existing: HashSet<String>,
    /// Ids with a row that is not in the trash.
    live: HashSet<String>,
}

impl LockedTargets {
    /// Whether an increment of `id` is refused under `on_missing`.
    fn refuses(&self, id: &str, on_missing: MissingTarget) -> bool {
        match on_missing {
            MissingTarget::Reject => !self.live.contains(id),
            MissingTarget::RejectMissing => !self.existing.contains(id),
            MissingTarget::Skip => false,
        }
    }
}

/// [`apply_deltas`] with an explicit missing-target policy.
///
/// Two concurrent writes that share targets must lock them in the same order,
/// or each can hold one the other waits on (a Postgres deadlock). So the
/// targets are first locked in ONE pass over all of them — collections in
/// name order, ids in id order within each collection — before any count
/// moves. That pass is also the existence / trash check an increment is
/// judged by, so a target cannot vanish or move to the trash between the
/// check and the count. Only then are the counts adjusted, on rows this
/// transaction already holds.
///
/// `SQLite` serializes writers through the caller's `IMMEDIATE` transaction,
/// so the same pass there only reads.
pub(super) fn apply_deltas_with(
    conn: &dyn DbConnection,
    deltas: &HashMap<(String, String), i64>,
    on_missing: MissingTarget,
) -> Result<()> {
    if deltas.is_empty() {
        return Ok(());
    }

    let sorted = sort_deltas(deltas);

    let mut locked = Vec::with_capacity(sorted.len());
    for (collection, targets) in &sorted {
        locked.push(lock_targets(conn, collection, targets)?);
    }

    for ((collection, targets), found) in sorted.iter().zip(&locked) {
        judge_increments(collection, targets, found, on_missing)?;
    }

    for ((collection, targets), found) in sorted.iter().zip(&locked) {
        update_counts(conn, collection, targets, found)?;
    }

    Ok(())
}

/// Order the delta map by collection, then by id.
fn sort_deltas(deltas: &HashMap<(String, String), i64>) -> SortedDeltas<'_> {
    let mut sorted = SortedDeltas::new();

    for ((collection, id), delta) in deltas {
        sorted
            .entry(collection.as_str())
            .or_default()
            .insert(id.as_str(), *delta);
    }

    sorted
}

/// Lock one collection's targets in id order (Postgres) and report which
/// exist and which are live.
///
/// Reads every column rather than naming `_deleted_at`, so it answers for a
/// collection without a trash column too.
fn lock_targets(
    conn: &dyn DbConnection,
    collection: &str,
    targets: &BTreeMap<&str, i64>,
) -> Result<LockedTargets> {
    let in_clause = placeholder_list(conn, targets.len());
    let lock = if conn.is_postgres() {
        " FOR UPDATE"
    } else {
        ""
    };
    let sql = format!("SELECT * FROM \"{collection}\" WHERE id IN ({in_clause}) ORDER BY id{lock}");
    let params: Vec<DbValue> = targets
        .keys()
        .map(|id| DbValue::Text((*id).to_string()))
        .collect();

    let rows = conn
        .query_all(&sql, &params)
        .with_context(|| format!("Failed to read reference targets in {collection}"))?;

    let mut found = LockedTargets::default();

    for row in &rows {
        let Ok(id) = row.get_string("id") else {
            continue;
        };

        if row.get_named("_deleted_at").is_none_or(DbValue::is_null) {
            found.live.insert(id.clone());
        }

        found.existing.insert(id);
    }

    Ok(found)
}

/// Refuse the write when an increment points at a target `on_missing` does
/// not allow — before any count of the write has moved.
fn judge_increments(
    collection: &str,
    targets: &BTreeMap<&str, i64>,
    found: &LockedTargets,
    on_missing: MissingTarget,
) -> Result<()> {
    let refused: Vec<String> = targets
        .iter()
        .filter(|(id, delta)| **delta > 0 && found.refuses(id, on_missing))
        .map(|(id, _)| (*id).to_string())
        .collect();

    if refused.is_empty() {
        return Ok(());
    }

    Err(UnavailableReferences {
        collection: collection.to_string(),
        ids: refused,
    }
    .into())
}

/// Adjust the counts of one collection's existing targets: one `UPDATE` per
/// distinct delta. A target with no row is left alone (see
/// [`report_missing`]).
fn update_counts(
    conn: &dyn DbConnection,
    collection: &str,
    targets: &BTreeMap<&str, i64>,
    found: &LockedTargets,
) -> Result<()> {
    let mut by_delta: BTreeMap<i64, Vec<&str>> = BTreeMap::new();

    for (id, delta) in targets {
        if found.existing.contains(*id) {
            by_delta.entry(*delta).or_default().push(*id);
            continue;
        }

        report_missing(collection, id, *delta);
    }

    for (delta, ids) in &by_delta {
        update_by(conn, collection, *delta, ids)?;
    }

    Ok(())
}

/// A target with no row that got past [`judge_increments`]: an increment is
/// only left for a repair replay (the reference is already stored and
/// dangling; refusing would block startup on data it cannot fix). A
/// decrement is a no-op — soft delete never decrements, so a missing row
/// means a hard delete already removed it.
fn report_missing(collection: &str, id: &str, delta: i64) {
    if delta > 0 {
        warn!(
            "Ref-count backfill: {collection}/{id} no longer exists — a dangling \
             reference was skipped (clear or update the referencing document)"
        );

        return;
    }

    debug!("Skipped decrement on {collection}/{id}: already gone");
}

/// `UPDATE` the counts of `ids` by `delta`, clamped at zero.
fn update_by(conn: &dyn DbConnection, collection: &str, delta: i64, ids: &[&str]) -> Result<()> {
    let in_clause = placeholder_list(conn, ids.len());
    let clamped = conn.greatest_expr("0", &format!("_ref_count + ({delta})"));
    let sql =
        format!("UPDATE \"{collection}\" SET _ref_count = {clamped} WHERE id IN ({in_clause})");
    let params: Vec<DbValue> = ids
        .iter()
        .map(|id| DbValue::Text((*id).to_string()))
        .collect();

    let affected = conn
        .execute(&sql, &params)
        .with_context(|| format!("Failed to batch-update _ref_count on {collection} by {delta}"))?;

    trace!("Adjusted _ref_count on {affected} target(s) in {collection} by {delta}");

    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CollectionDefinition;
    use crate::db::query::ref_count::outgoing_ref::OutgoingRef;
    use crate::db::query::ref_count::test_helpers::*;
    use crate::db::query::test_helpers::CountingConn;

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

    /// Regression: the targets were locked per `(collection, delta sign)`
    /// group in hash-map order, a fresh random order per call — so two
    /// concurrent writes sharing targets in two collections locked them in
    /// opposite orders and deadlocked on Postgres. Every target is now read
    /// (locked, on Postgres) in one pass sorted by collection, then id —
    /// increments and decrements alike — before any count moves.
    #[test]
    fn apply_deltas_locks_every_target_in_one_sorted_pass_before_any_update() {
        let defs = ["users", "media", "tags"].map(CollectionDefinition::new);
        let (_tmp, pool, _) = setup_db(&defs, &no_locale());
        let conn = pool.get().unwrap();

        for (collection, id) in [
            ("users", "u1"),
            ("media", "m2"),
            ("media", "m1"),
            ("tags", "t1"),
        ] {
            insert_doc(&conn, collection, id);
        }
        conn.execute("UPDATE media SET _ref_count = 1 WHERE id = 'm2'", &[])
            .unwrap();

        let deltas = HashMap::from([
            (("users".to_string(), "u1".to_string()), 1i64),
            (("tags".to_string(), "t1".to_string()), 1),
            (("media".to_string(), "m2".to_string()), -1),
            (("media".to_string(), "m1".to_string()), 1),
        ]);

        let counting = CountingConn::new(&conn);
        apply_deltas(&counting, &deltas).unwrap();

        let statements = counting.statements();
        let reads: Vec<&String> = statements.iter().take(3).collect();

        assert_eq!(statements.len(), 3 + 4, "3 lock reads, then 4 updates");
        for (read, collection) in reads.iter().zip(["media", "tags", "users"]) {
            assert!(
                read.starts_with(&format!("SELECT * FROM \"{collection}\"")),
                "targets are read collection by collection in name order: {statements:?}"
            );
            assert!(read.contains("ORDER BY id"), "ids in id order: {read}");
        }
        assert!(
            statements[3..].iter().all(|sql| sql.starts_with("UPDATE")),
            "no count moves before every target is read: {statements:?}"
        );

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
        assert_eq!(get_ref_count_val(&conn, "media", "m2"), 0);
        assert_eq!(get_ref_count_val(&conn, "tags", "t1"), 1);
        assert_eq!(get_ref_count_val(&conn, "users", "u1"), 1);
    }

    /// A refused increment is judged by the lock pass, so it stops the write
    /// before any count of the write moved — including the counts of
    /// collections that sort before it.
    #[test]
    fn apply_deltas_refuses_before_any_count_moves() {
        let defs = ["media", "tags"].map(CollectionDefinition::new);
        let (_tmp, pool, _) = setup_db(&defs, &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");

        let deltas = HashMap::from([
            (("media".to_string(), "m1".to_string()), 1i64),
            (("tags".to_string(), "t_missing".to_string()), 1),
        ]);

        let counting = CountingConn::new(&conn);
        let err = apply_deltas(&counting, &deltas).unwrap_err();

        assert!(err.downcast_ref::<UnavailableReferences>().is_some());
        assert!(counting.executed().is_empty(), "no UPDATE ran");
        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    /// The repair replay skips a dangling increment and still counts the
    /// targets that exist.
    #[test]
    fn repair_replay_skips_a_missing_target_and_counts_the_rest() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");

        let deltas = HashMap::from([
            (("media".to_string(), "m1".to_string()), 1i64),
            (("media".to_string(), "m_missing".to_string()), 1),
        ]);

        apply_deltas_with(&conn, &deltas, MissingTarget::Skip).expect("a dangling ref is skipped");

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
    }
}
