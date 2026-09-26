//! Ref-count maintenance for a created document: every reference it holds
//! is new.
//!
//! Applying a delta locks every referenced target first, in one pass sorted
//! by collection and id (see `delta::apply_deltas_with`), so two writes that
//! share targets take their locks in the same order.

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::{DocumentFields, FieldDefinition, flatten_group_fields},
    db::{
        DbConnection,
        query::ref_count::{
            added::AddedReferences,
            compute::compute_refs_from_data,
            delta::{MissingTarget, apply_deltas, apply_deltas_with, to_delta_map},
            read::read_outgoing_refs,
        },
    },
};

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

/// Replay a document's outgoing refs during the ref-count backfill.
///
/// Identical to [`after_create`] except that a reference whose target no
/// longer exists is skipped with a warning instead of failing: the backfill
/// runs inside the startup migration, and a single dangling reference (left
/// by a crash between a hard delete and its ref-count update, or by direct
/// SQL) must not make the server refuse to start on data it cannot repair.
///
/// # Errors
///
/// Returns a backend error if reading outgoing refs or the UPDATEs fail.
pub fn backfill_after_create(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    let new_refs = read_outgoing_refs(conn, table, id, fields, locale_config)?;
    let deltas = to_delta_map(&[], &new_refs);

    apply_deltas_with(conn, &deltas, MissingTarget::Skip)
}

/// Adjust ref counts after creating a new document — data-driven variant.
///
/// Instead of reading outgoing refs back from the DB (which wastes 5+ round-trips
/// for data that was just written), computes refs directly from the write data.
/// This eliminates all SELECT queries from the create path's ref count phase.
///
/// Returns the references the create adds (every one it holds), for a caller
/// that judges them further.
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
) -> Result<AddedReferences> {
    let mut new_refs = Vec::new();

    // DB-layer edge: flatten the canonical nested write data (idempotent) so the
    // in-memory ref walk reads the flat `group__sub` columns it expects.
    let data = flatten_group_fields(data, fields);
    compute_refs_from_data(fields, &data, "", &mut new_refs);

    let deltas = to_delta_map(&[], &new_refs);

    apply_deltas(conn, &deltas)?;

    Ok(AddedReferences::between(&[], &new_refs))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        core::{CollectionDefinition, Slug, field::*},
        db::query::ref_count::test_helpers::*,
    };

    fn upload_field() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ]
    }

    // ── after_create ─────────────────────────────────────────────────────

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

    // ── after_create_from_data ──────────────────────────────────────────

    #[test]
    fn after_create_from_data_has_one() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field();
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
        let fields = upload_field();
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
        let fields = upload_field();
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
