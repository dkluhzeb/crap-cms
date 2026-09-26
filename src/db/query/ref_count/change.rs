//! Ref-count maintenance for a changed or deleted document: the references
//! it held before against those it holds after.
//!
//! Applying a delta locks every referenced target first, in one pass sorted
//! by collection and id (see `delta::apply_deltas_with`), so two writes that
//! share targets take their locks in the same order. A delete reads the
//! count under the same row lock (`get_ref_count_locked`), so a reference and
//! a concurrent delete of its target serialize — the loser sees the target
//! gone (the write fails) or the new count (the delete is refused).

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::FieldDefinition,
    db::{
        DbConnection,
        query::ref_count::{
            added::AddedReferences,
            delta::{MissingTarget, apply_deltas, apply_deltas_with, to_delta_map},
            outgoing_ref::OutgoingRef,
            read::read_outgoing_refs,
        },
    },
};

/// Adjust ref counts before hard-deleting a document.
/// Reads current outgoing refs and decrements targets.
/// Must be called BEFORE the DELETE (CASCADE would remove junction rows).
///
/// # Errors
///
/// Returns a backend error if reading outgoing refs or applying deltas fails.
pub fn before_hard_delete(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    let old_refs = read_outgoing_refs(conn, table, id, fields, locale_config)?;
    let deltas = to_delta_map(&old_refs, &[]);

    apply_deltas(conn, &deltas)
}

/// Adjust ref counts around an update.
/// Reads outgoing refs before and after, then applies the diff.
///
/// The caller must pass `old_refs` obtained before the mutation. Returns the
/// references the update adds — those not among `old_refs` — for a caller
/// that judges them further.
///
/// # Errors
///
/// Returns a backend error if reading the new refs or applying deltas fails.
pub fn after_update(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
    old_refs: &[OutgoingRef],
) -> Result<AddedReferences> {
    let new_refs = read_outgoing_refs(conn, table, id, fields, locale_config)?;
    let deltas = to_delta_map(old_refs, &new_refs);

    apply_deltas(conn, &deltas)?;

    Ok(AddedReferences::between(old_refs, &new_refs))
}

/// Adjust ref counts around an import's write of a document: [`after_update`]
/// replaying the references the document was exported with. A missing target
/// is refused; a trashed one is kept — the reference was stored before its
/// target was trashed, and the export carries the target trashed.
///
/// # Errors
///
/// Returns a backend error if reading the new refs or applying deltas fails,
/// or [`UnavailableReferences`](super::UnavailableReferences) for a missing
/// target.
pub fn after_import(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
    old_refs: &[OutgoingRef],
) -> Result<()> {
    let new_refs = read_outgoing_refs(conn, table, id, fields, locale_config)?;
    let deltas = to_delta_map(old_refs, &new_refs);

    apply_deltas_with(conn, &deltas, MissingTarget::RejectMissing)
}

/// Snapshot the current outgoing refs for a document (call before mutation).
///
/// # Errors
///
/// Returns a backend error if reading outgoing refs fails.
pub fn snapshot_outgoing_refs(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<Vec<OutgoingRef>> {
    read_outgoing_refs(conn, table, id, fields, locale_config)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::{
        core::{CollectionDefinition, field::*},
        db::query::{
            join::{find_array_rows, set_array_rows},
            ref_count::{after_create, test_helpers::*},
        },
    };

    fn upload_field() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ]
    }

    // ── before_hard_delete ───────────────────────────────────────────────

    #[test]
    fn before_hard_delete_decrements_has_one() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");

        conn.execute("UPDATE media SET _ref_count = 1 WHERE id = 'm1'", &[])
            .unwrap();

        before_hard_delete(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    #[test]
    fn ref_count_does_not_go_negative() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");

        before_hard_delete(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    /// A diff-based array update that OMITS a relationship sub-field preserves
    /// that relationship in the DB, so `after_update` (which re-reads the
    /// persisted state) must leave the target's ref count untouched — never
    /// decrement a reference the write only appeared to drop. Guards the
    /// row-identity writer against silently under-counting (→ delete-protection
    /// bypass / dangling reference).
    #[test]
    fn array_update_omitting_preserved_relationship_keeps_ref_count() {
        let media = CollectionDefinition::new("media");
        let image = FieldDefinition::builder("image", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build();
        let slides = FieldDefinition::builder("slides", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("caption", FieldType::Text).build(),
                image,
            ])
            .build();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![slides];
        let fields = posts.fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "posts", "p1");

        // Create one slide referencing m1, then count refs → m1 = 1.
        let sub = &fields[0].fields;
        let rows = vec![HashMap::from([
            ("caption".to_string(), json!("hero")),
            ("image".to_string(), json!("m1")),
        ])];
        set_array_rows(&conn, "posts", "slides", "p1", &rows, sub, None).unwrap();
        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();
        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);

        let slide_id = find_array_rows(&conn, "posts", "slides", "p1", sub, None).unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // Update the slide by id, changing `caption` and OMITTING `image`
        // (as the write-access strip would for a denied relationship).
        let old_refs = snapshot_outgoing_refs(&conn, "posts", "p1", &fields, &no_locale()).unwrap();
        let update = vec![HashMap::from([
            ("id".to_string(), json!(slide_id)),
            ("caption".to_string(), json!("hero 2")),
        ])];
        set_array_rows(&conn, "posts", "slides", "p1", &update, sub, None).unwrap();
        after_update(&conn, "posts", "p1", &fields, &no_locale(), &old_refs).unwrap();

        assert_eq!(
            get_ref_count_val(&conn, "media", "m1"),
            1,
            "a preserved relationship must NOT be decremented by an update that omits it"
        );
    }

    // ── Update (swap ref) ────────────────────────────────────────────────

    #[test]
    fn after_update_swaps_ref_counts() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "media", "m2");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");

        // Simulate: set m1 ref_count to 1
        conn.execute("UPDATE media SET _ref_count = 1 WHERE id = 'm1'", &[])
            .unwrap();

        // Snapshot before update
        let old_refs = snapshot_outgoing_refs(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        // Simulate update: change image from m1 to m2
        conn.execute("UPDATE posts SET image = 'm2' WHERE id = 'p1'", &[])
            .unwrap();

        after_update(&conn, "posts", "p1", &fields, &no_locale(), &old_refs).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
        assert_eq!(get_ref_count_val(&conn, "media", "m2"), 1);
    }

    // ── after_update clearing a reference ────────────────────────────────

    #[test]
    fn after_update_clearing_ref_decrements() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");

        conn.execute("UPDATE media SET _ref_count = 1 WHERE id = 'm1'", &[])
            .unwrap();

        // Snapshot before update
        let old_refs = snapshot_outgoing_refs(&conn, "posts", "p1", &fields, &no_locale()).unwrap();
        assert_eq!(old_refs.len(), 1);

        // Clear the reference
        conn.execute("UPDATE posts SET image = '' WHERE id = 'p1'", &[])
            .unwrap();

        after_update(&conn, "posts", "p1", &fields, &no_locale(), &old_refs).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    // ── before_hard_delete with has-many ──────────────────────────────────

    #[test]
    fn before_hard_delete_decrements_has_many() {
        let tags = CollectionDefinition::new("tags");
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[tags, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "tags", "t1");
        insert_doc(&conn, "tags", "t2");
        insert_doc(&conn, "posts", "p1");

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

        conn.execute("UPDATE tags SET _ref_count = 1 WHERE id = 't1'", &[])
            .unwrap();
        conn.execute("UPDATE tags SET _ref_count = 1 WHERE id = 't2'", &[])
            .unwrap();

        before_hard_delete(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "tags", "t1"), 0);
        assert_eq!(get_ref_count_val(&conn, "tags", "t2"), 0);
    }
}
