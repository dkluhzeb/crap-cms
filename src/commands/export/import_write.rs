//! Writing imported documents: the row, its join rows and its search index,
//! then the reference counts once every document exists, and the post-commit
//! settle on the CLI's infrastructure.

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Map, Value};

use crate::{
    commands::export::{
        import_accounts::{
            lacks_password, overwrites_account, replaced_session_version, revoke_replaced_sessions,
        },
        import_row::{ImportRow, ImportTarget, canonical_document, collect_import_columns},
    },
    core::{nul_character_errors, validate::ValidationError},
    db::{
        DbConnection, LocaleContext, LocaleMode, UpsertSpec,
        query::{
            self,
            fts::{FtsIndex, fts_upsert},
            ref_count::OutgoingRef,
        },
    },
    service::{AppInfra, ServiceContext},
};

/// One collection's documents in the export, and where they go.
pub(super) struct ImportBatch<'a> {
    pub(super) target: ImportTarget<'a>,
    pub(super) docs: &'a [Value],
}

/// A written document. Its reference counts are settled once every document
/// of the import exists.
pub(super) struct Imported<'a> {
    pub(super) target: &'a ImportTarget<'a>,
    id: String,
    old_refs: Vec<OutgoingRef>,
    /// An account left without a password: it can't log in until one is set.
    pub(super) lacks_password: bool,
    /// The export carried the account's credentials.
    pub(super) carried_credentials: bool,
    /// The document replaced an account that already existed.
    overwrote_account: bool,
}

impl Imported<'_> {
    pub(super) fn settle_ref_counts(&self, tx: &dyn DbConnection) -> Result<()> {
        let target = self.target;

        query::ref_count::after_import(
            tx,
            target.slug,
            &self.id,
            &target.def.fields,
            target.locale,
            &self.old_refs,
        )
        .with_context(|| {
            format!(
                "Failed to update ref counts for {} in '{}'",
                self.id, target.slug
            )
        })
    }
}

/// Upsert the parent row by id; columns the import doesn't carry keep their
/// stored values.
fn upsert_row(tx: &dyn DbConnection, slug: &str, id: &str, row: &ImportRow<'_>) -> Result<()> {
    let placeholders: Vec<String> = (0..row.parent_cols.len())
        .map(|i| tx.placeholder(i + 1))
        .collect();
    let col_refs: Vec<&str> = row.parent_cols.iter().map(String::as_str).collect();
    let values = placeholders.join(", ");

    let spec = UpsertSpec::builder(slug, "id")
        .columns(&col_refs, &values)
        .build();
    let sql = tx.build_upsert(&spec);

    tx.execute(&sql, &row.parent_vals)
        .with_context(|| format!("Failed to insert document {id} into '{slug}'"))?;

    Ok(())
}

/// Write a document's join rows under the ids they were exported with; a
/// localized join field's rows each under their own locale.
fn write_join_rows(
    tx: &dyn DbConnection,
    target: &ImportTarget<'_>,
    id: &str,
    row: &ImportRow<'_>,
) -> Result<()> {
    let (slug, fields) = (target.slug, &target.def.fields);

    if !row.join_data.is_empty() {
        query::restore_join_table_data(tx, slug, fields, id, &row.join_data, None)?;
    }

    for (locale, data) in &row.localized_joins {
        let ctx = LocaleContext {
            mode: LocaleMode::Single(locale.clone()),
            config: target.locale.clone(),
        };
        query::restore_join_table_data(tx, slug, fields, id, data, Some(&ctx))?;
    }

    Ok(())
}

/// A document's JSON object and its id.
fn document_id<'v>(doc_val: &'v Value, slug: &str) -> Result<(&'v Map<String, Value>, &'v str)> {
    let doc_obj = doc_val
        .as_object()
        .ok_or_else(|| anyhow!("Expected document object in '{slug}'"))?;

    let id = doc_obj
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Document missing 'id' in '{slug}'"))?;

    Ok((doc_obj, id))
}

/// Import a single document via upsert + join table data, and index it for
/// full-text search exactly as the service write path does. Array and blocks
/// rows keep their exported ids. The outgoing refs are snapshotted before the
/// write (empty for a new document); the caller settles the counts.
fn import_single_document<'a>(
    doc_val: &Value,
    target: &'a ImportTarget<'a>,
    tx: &dyn DbConnection,
) -> Result<Imported<'a>> {
    let (slug, def) = (target.slug, target.def);
    let (doc_obj, id) = document_id(doc_val, slug)?;

    let old_refs =
        query::ref_count::snapshot_outgoing_refs(tx, slug, id, &def.fields, target.locale)
            .with_context(|| format!("Failed to snapshot refs for {id} in '{slug}'"))?;
    let replaced_session = replaced_session_version(tx, target, doc_obj, id)?;
    let overwrote_account = overwrites_account(tx, target, id)?;

    write_document(tx, target, (doc_obj, id))?;

    if let Some(replaced) = replaced_session {
        revoke_replaced_sessions(tx, slug, id, replaced)?;
    }

    Ok(Imported {
        target,
        id: id.to_string(),
        old_refs,
        lacks_password: lacks_password(tx, target, id)?,
        carried_credentials: doc_obj.contains_key("_credentials"),
        overwrote_account,
    })
}

/// Write a document's row and join rows in the shape a write stores, and index
/// it for full-text search.
fn write_document(
    tx: &dyn DbConnection,
    target: &ImportTarget<'_>,
    (doc_obj, id): (&Map<String, Value>, &str),
) -> Result<()> {
    let (slug, def) = (target.slug, target.def);

    let doc = canonical_document(doc_obj, &def.fields);

    // An import writes rows directly, past the write path's gates, so it
    // applies the NUL rule itself: a stored NUL cannot move to Postgres and
    // breaks every JSON-path filter there.
    let nul = nul_character_errors(&doc, &def.fields, &target.locale.locales);

    if !nul.is_empty() {
        return Err(ValidationError::new(nul))
            .with_context(|| format!("Document {id} in '{slug}' holds a NUL character"));
    }

    let row = collect_import_columns(&doc, target, id)?;

    upsert_row(tx, slug, id, &row)?;
    write_join_rows(tx, target, id, &row)?;

    if tx.supports_fts() {
        let index = FtsIndex::builder(slug, def, target.locale)
            .registry(target.registry)
            .build();

        fts_upsert(tx, &index, id)
            .with_context(|| format!("Failed to index {id} in '{slug}' for search"))?;
    }

    Ok(())
}

/// Write every document, then settle reference counts once all of them exist:
/// a document may reference one later in the file — in another collection or
/// its own — which counting per document would reject as missing.
pub(super) fn import_batches<'a>(
    tx: &dyn DbConnection,
    batches: &'a [ImportBatch<'a>],
) -> Result<Vec<Imported<'a>>> {
    let mut imported = Vec::new();

    for batch in batches {
        for doc in batch.docs {
            imported.push(import_single_document(doc, &batch.target, tx)?);
        }
    }

    for document in &imported {
        document.settle_ref_counts(tx)?;
    }

    Ok(imported)
}

/// After the commit, on the CLI's infrastructure: clear the populate cache the
/// import made stale (with Redis, `serve`'s own), and tear down the live
/// streams of every account it overwrote — their access was resolved from
/// what the import replaced.
pub(super) fn settle_after_commit(infra: &AppInfra, imported: &[Imported<'_>]) {
    let ctx = ServiceContext::slug_only("").infra(infra).build();

    ctx.clear_cache();

    for doc in imported.iter().filter(|doc| doc.overwrote_account) {
        ctx.publish_user_invalidation(&doc.id);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use tempfile::{TempDir, tempdir};
    use tokio::time::timeout;

    use super::*;
    use crate::{
        admin::test_support::test_infra_with_events,
        config::{CrapConfig, DatabaseConfig, LocaleConfig},
        core::{
            CollectionDefinition, FieldDefinition, FieldType, Registry, RelationshipConfig,
            collection::Auth,
        },
        db::{DbPool, DbValue, migrate, pool},
    };

    /// A database synced for `collections` under `locale`.
    fn synced_db(
        collections: Vec<CollectionDefinition>,
        locale: &LocaleConfig,
    ) -> (TempDir, DbPool) {
        let tmp = tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

        let registry_shared = Registry::shared();
        {
            let mut reg = registry_shared.write().unwrap();
            for collection in collections {
                reg.register_collection(collection);
            }
        }
        let registry = (*Registry::snapshot(&registry_shared)).clone();
        migrate::sync_all(&db_pool, &registry, locale).expect("sync");

        (tmp, db_pool)
    }

    /// Import one document the way `import` does, with the default locale config.
    fn import_doc(tx: &dyn DbConnection, slug: &str, def: &CollectionDefinition, doc: &Value) {
        let locale = LocaleConfig::default();
        let target = ImportTarget::resolve(tx, slug, def, &locale).unwrap();

        import_single_document(doc, &target, tx)
            .unwrap()
            .settle_ref_counts(tx)
            .unwrap();
    }

    fn setup_media_posts() -> (TempDir, DbPool, CollectionDefinition) {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Relationship)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        let posts_def = posts.clone();

        let (tmp, db_pool) = synced_db(vec![media, posts], &LocaleConfig::default());

        (tmp, db_pool, posts_def)
    }

    /// Regression: imported relationships must adjust `_ref_count` — the
    /// raw upsert used to skip ref counting entirely, leaving imported
    /// references invisible to delete protection (and the backfill is
    /// version-gated, so it would never repair them).
    #[test]
    fn import_adjusts_ref_counts() {
        let (_tmp, db_pool, posts_def) = setup_media_posts();

        let mut conn = db_pool.get().unwrap();
        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();

        // New document referencing m1 → count goes to 1.
        let doc = json!({ "id": "p1", "image": "m1" });
        let tx = conn.transaction().unwrap();
        import_doc(&tx, "posts", &posts_def, &doc);
        tx.commit().unwrap();

        let conn2 = db_pool.get().unwrap();
        assert_eq!(
            query::ref_count::get_ref_count(&conn2, "media", "m1").unwrap(),
            Some(1)
        );
        drop(conn2);

        // Re-import the same document unchanged → count stays 1 (upsert
        // diffs old vs new refs, no double counting).
        let tx = conn.transaction().unwrap();
        import_doc(&tx, "posts", &posts_def, &doc);
        tx.commit().unwrap();

        let conn2 = db_pool.get().unwrap();
        assert_eq!(
            query::ref_count::get_ref_count(&conn2, "media", "m1").unwrap(),
            Some(1)
        );
        drop(conn2);

        // Re-import with the reference cleared → count drops to 0.
        let doc_cleared = json!({ "id": "p1", "image": null });
        let tx = conn.transaction().unwrap();
        import_doc(&tx, "posts", &posts_def, &doc_cleared);
        tx.commit().unwrap();

        let conn2 = db_pool.get().unwrap();
        assert_eq!(
            query::ref_count::get_ref_count(&conn2, "media", "m1").unwrap(),
            Some(0)
        );
    }

    /// Regression: reference counts were applied per document, so a document
    /// referencing one later in the export — collections import in slug order —
    /// failed the whole import on a fresh database with "target no longer
    /// exists". Counts now settle once every document is written.
    #[test]
    fn a_reference_to_a_document_later_in_the_export_imports() {
        let (_tmp, db_pool, posts_def) = setup_media_posts();
        let media_def = CollectionDefinition::new("media");
        let locale = LocaleConfig::default();

        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();

        let posts_docs = [json!({ "id": "p1", "image": "m1" })];
        let media_docs = [json!({ "id": "m1" })];
        let batches = [
            ImportBatch {
                target: ImportTarget::resolve(&tx, "posts", &posts_def, &locale).unwrap(),
                docs: &posts_docs,
            },
            ImportBatch {
                target: ImportTarget::resolve(&tx, "media", &media_def, &locale).unwrap(),
                docs: &media_docs,
            },
        ];

        import_batches(&tx, &batches).expect("a forward reference must import");
        tx.commit().unwrap();

        let conn2 = db_pool.get().unwrap();
        assert_eq!(
            query::ref_count::get_ref_count(&conn2, "media", "m1").unwrap(),
            Some(1)
        );
    }

    /// Credentials that aren't an object are refused rather than skipped.
    #[test]
    fn non_object_credentials_are_rejected() {
        let (_tmp, db_pool, posts_def) = setup_media_posts();
        let locale = LocaleConfig::default();

        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let target = ImportTarget::resolve(&tx, "posts", &posts_def, &locale).unwrap();

        let doc = json!({ "id": "p1", "_credentials": "hash" });
        let err = import_single_document(&doc, &target, &tx)
            .err()
            .expect("non-object credentials must be refused")
            .to_string();

        assert!(err.contains("'_credentials' must be an object"), "{err}");
    }

    /// Regression (`SQLite` import upsert): re-importing a REFERENCED document must
    /// preserve its `_ref_count`. `INSERT OR REPLACE` deleted+reinserted the row,
    /// reverting `_ref_count` to its DDL default (0) and silently defeating
    /// delete protection (and diverging from Postgres). The `ON CONFLICT … DO
    /// UPDATE` upsert preserves columns not in the import set.
    #[test]
    fn reimporting_a_referenced_document_preserves_ref_count() {
        let (_tmp, db_pool, posts_def) = setup_media_posts();
        let media_def = CollectionDefinition::new("media");

        let mut conn = db_pool.get().unwrap();

        // Seed m1, then a post referencing it → m1._ref_count = 1.
        {
            let tx = conn.transaction().unwrap();
            import_doc(&tx, "media", &media_def, &json!({ "id": "m1" }));
            import_doc(
                &tx,
                "posts",
                &posts_def,
                &json!({ "id": "p1", "image": "m1" }),
            );
            tx.commit().unwrap();
        }

        let conn2 = db_pool.get().unwrap();
        assert_eq!(
            query::ref_count::get_ref_count(&conn2, "media", "m1").unwrap(),
            Some(1),
            "precondition: m1 is referenced once"
        );
        drop(conn2);

        // Re-import m1 (the referenced doc). Its _ref_count must survive.
        let tx = conn.transaction().unwrap();
        import_doc(&tx, "media", &media_def, &json!({ "id": "m1" }));
        tx.commit().unwrap();

        let conn2 = db_pool.get().unwrap();
        assert_eq!(
            query::ref_count::get_ref_count(&conn2, "media", "m1").unwrap(),
            Some(1),
            "re-importing the referenced doc must NOT reset its _ref_count"
        );
    }

    /// Regression: an imported document must be indexed for full-text search.
    /// The raw upsert wrote the row but skipped `fts_upsert`, so imported docs
    /// were invisible to search. Import now re-reads the row and indexes it like
    /// the service write path.
    #[test]
    fn import_indexes_document_for_search() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        let posts_def = posts.clone();

        let (_tmp, db_pool) = synced_db(vec![media, posts], &LocaleConfig::default());

        let mut conn = db_pool.get().unwrap();
        let doc = json!({ "id": "p1", "title": "searchable haystack" });
        let tx = conn.transaction().unwrap();
        import_doc(&tx, "posts", &posts_def, &doc);
        tx.commit().unwrap();

        let conn2 = db_pool.get().unwrap();
        let hits = conn2
            .query_all(
                "SELECT id FROM _fts_posts WHERE _fts_posts MATCH 'haystack'",
                &[],
            )
            .expect("FTS query");
        let ids: Vec<String> = hits
            .iter()
            .filter_map(|r| match r.get_value(0) {
                Some(DbValue::Text(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["p1"], "imported document must be searchable");
    }

    /// An imported address or text value is stored in the same canonical form
    /// a write through the API stores, so the account can log in and uniqueness
    /// holds against later writes.
    #[test]
    fn import_stores_canonical_email_and_text() {
        let mut people = CollectionDefinition::new("people");
        people.fields = vec![
            FieldDefinition::builder("email", FieldType::Email).build(),
            FieldDefinition::builder("name", FieldType::Text).build(),
        ];
        let people_def = people.clone();

        let (_tmp, db_pool) = synced_db(vec![people], &LocaleConfig::default());

        let mut conn = db_pool.get().unwrap();
        let doc =
            json!({ "id": "p1", "email": " ANGE\u{300}LE@Example.com", "name": "Rene\u{301}" });
        let tx = conn.transaction().unwrap();
        import_doc(&tx, "people", &people_def, &doc);
        tx.commit().unwrap();

        let row = conn
            .query_one("SELECT email, name FROM people WHERE id = 'p1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("email").unwrap(), "ang\u{e8}le@example.com");
        assert_eq!(row.get_string("name").unwrap(), "Ren\u{e9}");
    }

    /// Regression: an import wrote rows directly, past the write path's NUL
    /// rule, so a NUL in an exported row's JSON (from a `SQLite` database) was
    /// stored — and on Postgres broke every row-path filter on the collection.
    #[test]
    fn a_nul_inside_a_row_is_refused() {
        let mut notes = CollectionDefinition::new("notes");
        notes.fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                ])
                .build(),
        ];
        let notes_def = notes.clone();

        let (_tmp, db_pool) = synced_db(vec![notes], &LocaleConfig::default());

        let mut conn = db_pool.get().unwrap();
        let doc = json!({ "id": "n1", "items": [{ "label": "a\u{0}b" }] });
        let tx = conn.transaction().unwrap();
        let locale = LocaleConfig::default();
        let target = ImportTarget::resolve(&tx, "notes", &notes_def, &locale).unwrap();

        let Err(err) = import_single_document(&doc, &target, &tx) else {
            panic!("a NUL is refused");
        };

        assert!(format!("{err:#}").contains("items[0][label]"), "{err:#}");
    }

    fn localized_title() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
        ]
    }

    /// Sync one `posts` collection of `fields` under `locale` and import `doc`
    /// into it.
    fn import_posts(
        fields: Vec<FieldDefinition>,
        locale: &LocaleConfig,
        doc: &Value,
    ) -> Result<(TempDir, DbPool)> {
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields;
        let def = posts.clone();

        let (tmp, db_pool) = synced_db(vec![posts], locale);

        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let target = ImportTarget::resolve(&tx, "posts", &def, locale)?;
        import_single_document(doc, &target, &tx)?.settle_ref_counts(&tx)?;
        tx.commit()?;
        drop(conn);

        Ok((tmp, db_pool))
    }

    /// A hyphenated locale's value lands in its column (`title__pt_BR`), not a
    /// `title__pt-BR` column that doesn't exist.
    #[test]
    fn a_hyphenated_locale_imports_into_its_column() {
        let locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "pt-BR".to_string()],
            fallback: true,
        };
        let doc = json!({ "id": "p1", "title": { "en": "Hello", "pt-BR": "Ol\u{e1}" } });

        let (_tmp, db_pool) = import_posts(localized_title(), &locale, &doc).unwrap();

        let row = db_pool
            .get()
            .unwrap()
            .query_one("SELECT title__pt_BR FROM posts WHERE id = 'p1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("title__pt_BR").unwrap(), "Ol\u{e1}");
    }

    /// With locales off a localized field has one column and exports a bare
    /// value, so its import takes the bare value instead of demanding an
    /// object of locales.
    #[test]
    fn a_localized_field_imports_a_bare_value_with_locales_off() {
        let doc = json!({ "id": "p1", "title": "Hello" });

        let (_tmp, db_pool) =
            import_posts(localized_title(), &LocaleConfig::default(), &doc).unwrap();

        let row = db_pool
            .get()
            .unwrap()
            .query_one("SELECT title FROM posts WHERE id = 'p1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("title").unwrap(), "Hello");
    }

    /// Rows exported per locale can't become one set of rows with locales
    /// off; the import stops instead of storing the locale object as rows.
    #[test]
    fn rows_per_locale_are_refused_with_locales_off() {
        let slides = FieldDefinition::builder("slides", FieldType::Array)
            .localized(true)
            .fields(vec![
                FieldDefinition::builder("caption", FieldType::Text).build(),
            ])
            .build();
        let doc = json!({ "id": "p1", "slides": { "en": [{ "caption": "A" }] } });

        let Err(err) = import_posts(vec![slides], &LocaleConfig::default(), &doc) else {
            panic!("rows per locale must be refused with locales off");
        };
        let err = err.to_string();

        assert!(
            err.contains("slides") && err.contains("per locale"),
            "{err}"
        );
    }

    fn users_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth::new(true));
        def.fields = vec![FieldDefinition::builder("email", FieldType::Email).build()];

        def
    }

    /// Regression: an import that overwrote an account left its open
    /// live-update streams running on the access the import replaced, and
    /// never cleared the cache. Overwritten accounts are now flagged, and the
    /// post-commit settle clears the cache and ends their streams.
    #[tokio::test]
    async fn an_overwritten_account_has_its_streams_ended_after_the_commit() {
        let def = users_def();
        let (_tmp, infra, _events) = test_infra_with_events(def.clone());
        let locale = LocaleConfig::default();

        infra.cache.set("populate:stale", b"x").unwrap();
        let mut invalidations = infra.invalidation_transport.subscribe();

        let mut conn = infra.pool.get().unwrap();
        conn.execute(
            "INSERT INTO users (id, email) VALUES ('u1', 'old@example.com')",
            &[],
        )
        .unwrap();

        let tx = conn.transaction().unwrap();
        let target = ImportTarget::resolve(&tx, "users", &def, &locale).unwrap();
        let docs = [
            json!({ "id": "u1", "email": "new@example.com" }),
            json!({ "id": "u2", "email": "fresh@example.com" }),
        ];
        let imported: Vec<Imported<'_>> = docs
            .iter()
            .map(|doc| import_single_document(doc, &target, &tx).unwrap())
            .collect();
        tx.commit().unwrap();

        assert!(imported[0].overwrote_account, "u1 existed");
        assert!(!imported[1].overwrote_account, "u2 is new");

        settle_after_commit(&infra, &imported);

        let ended = timeout(Duration::from_secs(1), invalidations.recv())
            .await
            .expect("recv timed out")
            .expect("an invalidation signal");
        assert_eq!(ended, "u1");
        assert!(
            infra.cache.get("populate:stale").unwrap().is_none(),
            "the import must clear the cache"
        );
    }
}
