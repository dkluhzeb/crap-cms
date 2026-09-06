//! Orchestrators and read-only entrypoints for ref-count maintenance.
//!
//! Public API used by the write/persist layer:
//! - [`get_ref_count`] / [`get_ref_count_locked`] — peek
//! - [`data_touches_refs`] — fast-path skip predicate for updates that don't
//!   touch any ref-bearing field
//! - [`after_create`] / [`after_create_from_data`] / [`after_update`] /
//!   [`before_hard_delete`] — apply deltas around a write
//! - [`snapshot_outgoing_refs`] — capture state for the update path
//! - [`lock_ref_targets_from_data`] — historical no-op kept for call-site
//!   stability

use anyhow::Result;

use crate::config::LocaleConfig;
use crate::core::{
    DocumentFields, FieldChildren, FieldDefinition, FieldType, any_field, field_children,
    flatten_group_fields,
};
use crate::db::query::helpers::prefixed_name;
use crate::db::{DbConnection, DbValue};

use super::compute::compute_refs_from_data;
use super::delta::{apply_deltas, to_delta_map};
use super::outgoing_ref::OutgoingRef;
use super::read::read_outgoing_refs;

/// Read the `_ref_count` value for a document.
/// Returns `None` if the document does not exist, `Some(count)` otherwise
/// (defaulting to 0 when the column is NULL).
///
/// # Errors
///
/// Returns a backend error if the SELECT query fails.
pub fn get_ref_count(conn: &dyn DbConnection, collection: &str, id: &str) -> Result<Option<i64>> {
    get_ref_count_inner(conn, collection, id, false)
}

/// Read `_ref_count` with a row-level lock (`SELECT ... FOR UPDATE` on Postgres).
///
/// Used by the delete path to prevent a concurrent create from incrementing the
/// ref count between the check and the actual DELETE. On `SQLite`, `IMMEDIATE`
/// transactions already serialize writes, so no lock suffix is needed.
///
/// # Errors
///
/// Returns a backend error if the SELECT query fails.
pub fn get_ref_count_locked(
    conn: &dyn DbConnection,
    collection: &str,
    id: &str,
) -> Result<Option<i64>> {
    get_ref_count_inner(conn, collection, id, true)
}

fn get_ref_count_inner(
    conn: &dyn DbConnection,
    collection: &str,
    id: &str,
    lock: bool,
) -> Result<Option<i64>> {
    let p1 = conn.placeholder(1);
    let for_update = if lock && conn.is_postgres() {
        " FOR UPDATE"
    } else {
        ""
    };
    let sql = format!("SELECT _ref_count FROM \"{collection}\" WHERE id = {p1}{for_update}");
    let row = conn.query_one(&sql, &[DbValue::Text(id.to_string())])?;

    Ok(row.map(|r| {
        r.get_value(0)
            .and_then(|v| match v {
                DbValue::Integer(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0)
    }))
}

/// Check whether write data contains any relationship or upload field values.
///
/// When an update doesn't touch any ref-bearing fields, the entire `ref_count`
/// dance (snapshot before, read after, apply deltas) can be skipped — saving
/// 10+ queries on the hot path.
#[must_use]
pub fn data_touches_refs(fields: &[FieldDefinition], data: &DocumentFields, prefix: &str) -> bool {
    // DB-layer edge: the walk reads flat `group__sub` columns, so flatten the
    // canonical nested write data first (idempotent).
    let data = flatten_group_fields(data, fields);

    data_touches_refs_inner(fields, &data, prefix)
}

fn data_touches_refs_inner(
    fields: &[FieldDefinition],
    data: &DocumentFields,
    prefix: &str,
) -> bool {
    for field in fields {
        match field_children(field) {
            FieldChildren::Group(sub) => {
                let new_prefix = prefixed_name(prefix, &field.name);
                if data_touches_refs_inner(sub, data, &new_prefix) {
                    return true;
                }
            }

            FieldChildren::Wrapper(sub) => {
                if data_touches_refs_inner(sub, data, prefix) {
                    return true;
                }
            }

            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    if data_touches_refs_inner(&tab.fields, data, prefix) {
                        return true;
                    }
                }
            }

            FieldChildren::Array(sub) => {
                let col = prefixed_name(prefix, &field.name);

                // Recurse the sub-field tree (groups/arrays/blocks at any
                // depth) so a relationship nested inside a group within the
                // array still arms ref-count recomputation.
                if data.contains_key(&col) && fields_contain_relationship(sub) {
                    return true;
                }
            }

            FieldChildren::Blocks(blocks) => {
                let col = prefixed_name(prefix, &field.name);

                if data.contains_key(&col)
                    && blocks
                        .iter()
                        .any(|b| fields_contain_relationship(&b.fields))
                {
                    return true;
                }
            }

            // Relationship/Upload leaves carry a stored reference — a write to
            // one arms ref-count recomputation. Scalars and the virtual Join
            // never do.
            FieldChildren::Leaf => {
                if matches!(
                    field.field_type,
                    FieldType::Relationship | FieldType::Upload
                ) {
                    let col = prefixed_name(prefix, &field.name);

                    if data.contains_key(&col) {
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// Recursively report whether any field in the subtree references another
/// collection — via the shared `any_field` container walk (descends
/// Group/Array/Blocks/Row/Collapsible/Tabs) and the `is_reference` classifier.
fn fields_contain_relationship(fields: &[FieldDefinition]) -> bool {
    any_field(fields, &|f| f.field_type.is_reference())
}

/// Historically this function pre-locked every outgoing ref target with
/// `SELECT ... FOR UPDATE` before the main INSERT on Postgres. That added
/// one round-trip per referenced document on the write hot path (3-5 per
/// typical create), and the serialization it provided was redundant — the
/// subsequent `UPDATE <target> SET _ref_count = _ref_count + 1 WHERE id = ?`
/// in [`apply_deltas`] already takes the same row-level write lock and the
/// `affected == 0` check already bails on a concurrently-deleted target,
/// rolling back the enclosing transaction. Removing the pre-lock roughly
/// doubles postgres write throughput under concurrent writes with shared
/// ref targets.
///
/// Kept as a no-op function so every existing call site (create, update,
/// version restore) stays wired up without churn; callers just don't pay
/// for an explicit pre-lock anymore.
///
/// `SQLite` has always been a no-op here (`IMMEDIATE` transactions serialize
/// all writers at the DB level), so `SQLite` behavior is unchanged.
///
/// # Errors
///
/// Currently infallible (returns `Ok(())` unconditionally). The `Result`
/// return type is preserved so the call sites can keep their `?` operators
/// in case future backends reintroduce a fallible pre-lock step.
pub fn lock_ref_targets_from_data(
    _conn: &dyn DbConnection,
    _fields: &[FieldDefinition],
    _data: &DocumentFields,
    _locale_config: &LocaleConfig,
) -> Result<()> {
    Ok(())
}

/// Adjust ref counts after creating a new document.
/// Reads the newly written outgoing refs and increments targets.
///
/// # Errors
///
/// Returns a backend error if reading outgoing refs or applying deltas fails.
pub fn after_create(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    let new_refs = read_outgoing_refs(conn, table, id, fields, locale_config)?;
    let deltas = to_delta_map(&[], &new_refs);

    apply_deltas(conn, &deltas)
}

/// Adjust ref counts after creating a new document — data-driven variant.
///
/// Instead of reading outgoing refs back from the DB (which wastes 5+ round-trips
/// for data that was just written), computes refs directly from the write data.
/// This eliminates all SELECT queries from the create path's ref count phase.
///
/// # Errors
///
/// Returns a backend error if applying deltas fails (e.g. target row missing).
pub fn after_create_from_data(
    conn: &dyn DbConnection,
    fields: &[FieldDefinition],
    data: &DocumentFields,
    // Kept for signature symmetry with the ref-count family (`after_create`,
    // `after_update`); the in-memory compute path is single-locale (creates
    // land in the default locale, translations are updates via the DB-read
    // path), so it reads bare data keys and never consults locale config.
    _locale_config: &LocaleConfig,
) -> Result<()> {
    let mut new_refs = Vec::new();

    // DB-layer edge: flatten the canonical nested write data (idempotent) so the
    // in-memory ref walk reads the flat `group__sub` columns it expects.
    let data = flatten_group_fields(data, fields);
    compute_refs_from_data(fields, &data, "", &mut new_refs);

    let deltas = to_delta_map(&[], &new_refs);

    apply_deltas(conn, &deltas)
}

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
/// The caller must pass `old_refs` obtained before the mutation.
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
) -> Result<()> {
    let new_refs = read_outgoing_refs(conn, table, id, fields, locale_config)?;
    let deltas = to_delta_map(old_refs, &new_refs);

    apply_deltas(conn, &deltas)
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
    use serde_json::json;

    use super::*;
    use crate::core::CollectionDefinition;
    use crate::core::DocumentFields;
    use crate::core::Slug;
    use crate::core::field::*;
    use crate::db::query::ref_count::test_helpers::*;

    fn upload_field() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ]
    }

    // ── get_ref_count ────────────────────────────────────────────────────

    #[test]
    fn ref_count_defaults_to_zero() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    /// Regression: `get_ref_count` must return None for missing documents
    /// instead of 0, so callers can distinguish "not found" from "zero refs".
    #[test]
    fn ref_count_returns_none_for_missing_document() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        let result = get_ref_count(&conn, "media", "nonexistent").unwrap();
        assert_eq!(result, None, "Missing document should return None");
    }

    // ── after_create / before_hard_delete integration ────────────────────

    #[test]
    fn after_create_increments_has_one() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
    }

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

    // ── Has-many relationship ────────────────────────────────────────────

    #[test]
    fn after_create_increments_has_many() {
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

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "tags", "t1"), 1);
        assert_eq!(get_ref_count_val(&conn, "tags", "t2"), 1);
    }

    // ── Polymorphic has-one ──────────────────────────────────────────────

    #[test]
    fn after_create_polymorphic_has_one() {
        let media = CollectionDefinition::new("media");
        let pages = CollectionDefinition::new("pages");
        let fields = vec![
            FieldDefinition::builder("featured", FieldType::Relationship)
                .relationship(RelationshipConfig {
                    collection: Slug::new("media"),
                    has_many: false,
                    max_depth: None,
                    polymorphic: vec![Slug::new("media"), Slug::new("pages")],
                })
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, pages, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "featured", "media/m1");

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
    }

    // ── Polymorphic has-many ─────────────────────────────────────────────

    #[test]
    fn after_create_polymorphic_has_many() {
        let media = CollectionDefinition::new("media");
        let pages = CollectionDefinition::new("pages");
        let fields = vec![
            FieldDefinition::builder("related", FieldType::Relationship)
                .relationship(RelationshipConfig {
                    collection: Slug::new("media"),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec![Slug::new("media"), Slug::new("pages")],
                })
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, pages, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "pages", "pg1");
        insert_doc(&conn, "posts", "p1");

        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES ('p1', 'm1', 'media', 0)",
            &[],
        ).unwrap();
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES ('p1', 'pg1', 'pages', 1)",
            &[],
        ).unwrap();

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
        assert_eq!(get_ref_count_val(&conn, "pages", "pg1"), 1);
    }

    // ── Localized has-one ────────────────────────────────────────────────

    #[test]
    fn after_create_localized_has_one() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("hero", FieldType::Upload)
                .localized(true)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        let locale = locale_en_de();

        let (_tmp, pool, _) = setup_db(&[media, posts], &locale);
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "media", "m2");

        conn.execute(
            "INSERT INTO posts (id, hero__en, hero__de) VALUES ('p1', 'm1', 'm2')",
            &[],
        )
        .unwrap();

        let fields = vec![
            FieldDefinition::builder("hero", FieldType::Upload)
                .localized(true)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        after_create(&conn, "posts", "p1", &fields, &locale).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
        assert_eq!(get_ref_count_val(&conn, "media", "m2"), 1);
    }

    /// Regression: the CREATE hot-path
    /// (`after_create_from_data` → the in-memory compute walker) must
    /// count a localized has-one ref. The write data is BARE-keyed
    /// (`hero`, single-locale mode), not `hero__en`/`hero__de` — the
    /// compute walker's localized branch read suffixed keys and missed
    /// the ref entirely, under-counting the target and bypassing delete
    /// protection. The existing `after_create_localized_has_one` above
    /// exercises only the READ path (`after_create`), so it never caught
    /// this.
    #[test]
    fn after_create_from_data_localized_has_one_counts_the_ref() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("hero", FieldType::Upload)
                .localized(true)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        posts.fields = fields.clone();
        let locale = locale_en_de();

        let (_tmp, pool, _) = setup_db(&[media, posts], &locale);
        let conn = pool.get().unwrap();
        insert_doc(&conn, "media", "m1");

        // Bare-keyed write data, exactly as the write path produces it in
        // single-locale mode.
        let mut data = DocumentFields::new();
        data.insert("hero".to_string(), json!("m1"));

        after_create_from_data(&conn, &fields, &data, &locale).unwrap();

        assert_eq!(
            get_ref_count_val(&conn, "media", "m1"),
            1,
            "localized has-one ref must be counted on the create hot-path"
        );
    }

    // ── Array sub-field refs ─────────────────────────────────────────────

    #[test]
    fn after_create_array_sub_field_refs() {
        let media = CollectionDefinition::new("media");
        let fields = vec![
            FieldDefinition::builder("slides", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("image", FieldType::Upload)
                        .relationship(RelationshipConfig::new("media", false))
                        .build(),
                ])
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "media", "m2");
        insert_doc(&conn, "posts", "p1");

        conn.execute(
            "INSERT INTO posts_slides (id, parent_id, _order, image) VALUES ('s1', 'p1', 0, 'm1')",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_slides (id, parent_id, _order, image) VALUES ('s2', 'p1', 1, 'm2')",
            &[],
        )
        .unwrap();

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
        assert_eq!(get_ref_count_val(&conn, "media", "m2"), 1);
    }

    // ── Block sub-field refs ─────────────────────────────────────────────

    #[test]
    fn after_create_blocks_sub_field_refs() {
        let media = CollectionDefinition::new("media");
        let fields = vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "hero",
                    vec![
                        FieldDefinition::builder("bg_image", FieldType::Upload)
                            .relationship(RelationshipConfig::new("media", false))
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "posts", "p1");

        conn.execute(
            "INSERT INTO posts_content (id, parent_id, _order, _block_type, data) VALUES ('b1', 'p1', 0, 'hero', '{\"bg_image\":\"m1\"}')",
            &[],
        ).unwrap();

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
    }

    // ── Group nesting ────────────────────────────────────────────────────

    #[test]
    fn after_create_group_nested_ref() {
        let media = CollectionDefinition::new("media");
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("hero", FieldType::Upload)
                        .relationship(RelationshipConfig::new("media", false))
                        .build(),
                ])
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "meta__hero", "m1");

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
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

    // ── Multiple fields referencing same target ──────────────────────────

    #[test]
    fn multiple_fields_same_target() {
        let media = CollectionDefinition::new("media");
        let fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
            FieldDefinition::builder("thumbnail", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        conn.execute(
            "INSERT INTO posts (id, image, thumbnail) VALUES ('p1', 'm1', 'm1')",
            &[],
        )
        .unwrap();

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        // Same target referenced by two fields = ref_count 2
        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 2);
    }

    // ── Empty/null has-one column ────────────────────────────────────────

    #[test]
    fn empty_has_one_yields_no_refs() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        // Insert post with NULL image (no value provided)
        insert_doc(&conn, "posts", "p1");

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        // No ref should be created for NULL/empty
        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
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

    // ── after_create_from_data ──────────────────────────────────────────

    fn upload_field_for_data() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ]
    }

    #[test]
    fn after_create_from_data_has_one() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field_for_data();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");

        let mut data = DocumentFields::new();
        data.insert("image".to_string(), json!("m1"));

        after_create_from_data(&conn, &fields, &data, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
    }

    #[test]
    fn after_create_from_data_has_many() {
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

        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(["t1", "t2"]));

        after_create_from_data(&conn, &fields, &data, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "tags", "t1"), 1);
        assert_eq!(get_ref_count_val(&conn, "tags", "t2"), 1);
    }

    #[test]
    fn after_create_from_data_polymorphic_has_many() {
        let articles = CollectionDefinition::new("articles");
        let pages = CollectionDefinition::new("pages");
        let mut rc = RelationshipConfig::new("articles", true);
        rc.polymorphic = vec!["articles".into(), "pages".into()];
        let fields = vec![
            FieldDefinition::builder("refs", FieldType::Relationship)
                .relationship(rc)
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[articles, pages, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "articles", "a1");
        insert_doc(&conn, "pages", "pg1");

        let mut data = DocumentFields::new();
        data.insert("refs".to_string(), json!(["articles/a1", "pages/pg1"]));

        after_create_from_data(&conn, &fields, &data, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "articles", "a1"), 1);
        assert_eq!(get_ref_count_val(&conn, "pages", "pg1"), 1);
    }

    #[test]
    fn after_create_from_data_empty_values_no_refs() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field_for_data();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");

        // Empty string = no ref
        let mut data = DocumentFields::new();
        data.insert("image".to_string(), json!(""));

        after_create_from_data(&conn, &fields, &data, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    /// Regression: duplicate IDs in has-many data must be deduplicated
    /// (matching the DB path's SELECT DISTINCT). Without dedup, _`ref_count`
    /// would be inflated, permanently blocking deletion of the target.
    #[test]
    fn after_create_from_data_deduplicates_has_many() {
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

        let mut data = DocumentFields::new();
        // Duplicate "t1" — must count as 1, not 2
        data.insert("tags".to_string(), json!(["t1", "t1", "t1"]));

        after_create_from_data(&conn, &fields, &data, &no_locale()).unwrap();

        assert_eq!(
            get_ref_count_val(&conn, "tags", "t1"),
            1,
            "duplicate IDs must be deduplicated — ref_count should be 1, not 3"
        );
    }

    #[test]
    fn after_create_from_data_missing_target_fails() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field_for_data();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        let mut data = DocumentFields::new();
        data.insert("image".to_string(), json!("m_missing"));

        after_create_from_data(&conn, &fields, &data, &no_locale())
            .expect_err("should fail when target doesn't exist");
    }
}
