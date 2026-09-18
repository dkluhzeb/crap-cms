//! `import` command — load collection data from JSON.

use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value, from_str};

use crate::{
    cli,
    commands::{
        Project,
        export::{
            file::{EXPORT_FORMAT_VERSION, ExportFile},
            import_accounts::{lacks_password, replaced_session_version, revoke_replaced_sessions},
            import_row::{ImportRow, ImportTarget, canonical_document, collect_import_columns},
        },
        open_project,
    },
    config::{CrapConfig, LocaleConfig},
    core::{Registry, auth::open_totp_secret},
    db::{
        DbConnection, LocaleContext, LocaleMode, UpsertSpec,
        query::{self, ref_count::OutgoingRef},
    },
};

/// Where an import reads from: the export, and this installation's schema and
/// locales.
struct ImportSource<'a> {
    registry: &'a Registry,
    export_file: &'a ExportFile,
    locale: &'a LocaleConfig,
}

/// One collection's documents in the export, and where they go.
struct ImportBatch<'a> {
    target: ImportTarget<'a>,
    docs: &'a [Value],
}

/// A written document. Its reference counts are settled once every document
/// of the import exists.
struct Imported<'a> {
    target: &'a ImportTarget<'a>,
    id: String,
    old_refs: Vec<OutgoingRef>,
    /// An account left without a password: it can't log in until one is set.
    lacks_password: bool,
    /// The export carried the account's credentials.
    carried_credentials: bool,
}

impl Imported<'_> {
    fn settle_ref_counts(&self, tx: &dyn DbConnection) -> Result<()> {
        let target = self.target;

        query::ref_count::after_update(
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
    let row = collect_import_columns(&doc, target, id)?;

    upsert_row(tx, slug, id, &row)?;
    write_join_rows(tx, target, id, &row)?;

    if tx.supports_fts() {
        query::fts::fts_upsert(tx, slug, id, def, target.locale)
            .with_context(|| format!("Failed to index {id} in '{slug}' for search"))?;
    }

    Ok(())
}

/// Write every document, then settle reference counts once all of them exist:
/// a document may reference one later in the file — in another collection or
/// its own — which counting per document would reject as missing.
fn import_batches<'a>(
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

/// Refuse accounts whose TOTP secret doesn't open with this installation's
/// auth secret. It was sealed under another one, and an account whose secret
/// can't be read re-enrolls on its next login — letting whoever holds the
/// password register an authenticator of their own.
fn check_totp_secrets(batches: &[ImportBatch<'_>], auth_secret: &str) -> Result<()> {
    let sealed_elsewhere: Vec<String> = batches
        .iter()
        .filter(|batch| batch.target.credential_columns.contains(&"_totp_secret"))
        .flat_map(|batch| {
            batch.docs.iter().filter_map(move |doc| {
                let sealed = doc.pointer("/_credentials/_totp_secret")?.as_str()?;
                let id = doc.get("id").and_then(Value::as_str).unwrap_or("?");

                (open_totp_secret(auth_secret, sealed).is_none())
                    .then(|| format!("{}/{id}", batch.target.slug))
            })
        })
        .collect();

    if sealed_elsewhere.is_empty() {
        return Ok(());
    }

    bail!(
        "{} account(s) carry a TOTP secret sealed with a different auth secret: {}. Import into \
         an installation that uses the same auth secret (a backup carries a generated one), or \
         export without --include-credentials. Nothing was imported.",
        sealed_elsewhere.len(),
        sealed_elsewhere.join(", ")
    )
}

/// Refuse an export that lists a document twice in one collection: the later
/// copy would silently overwrite the earlier one.
fn check_duplicate_ids(batches: &[ImportBatch<'_>]) -> Result<()> {
    let mut duplicates = Vec::new();

    for batch in batches {
        let mut seen = HashSet::new();
        let ids = batch
            .docs
            .iter()
            .filter_map(|doc| doc.get("id").and_then(Value::as_str));

        for id in ids {
            if !seen.insert(id) {
                duplicates.push(format!("{}/{id}", batch.target.slug));
            }
        }
    }

    if duplicates.is_empty() {
        return Ok(());
    }

    bail!(
        "The export lists these documents more than once: {}. Nothing was imported.",
        duplicates.join(", ")
    )
}

/// Report what each collection imported, and the accounts left unable to log in.
fn report_import(batches: &[ImportBatch<'_>], imported: &[Imported<'_>]) {
    for batch in batches {
        let slug = batch.target.slug;
        cli::success(&format!(
            "Imported {} document(s) into '{slug}'",
            batch.docs.len()
        ));

        let without_password: Vec<&Imported<'_>> = imported
            .iter()
            .filter(|doc| doc.target.slug == slug && doc.lacks_password)
            .collect();

        if without_password.is_empty() {
            continue;
        }

        cli::warning(&format!(
            "{} account(s) in '{slug}' have no password and can't log in until one is set.",
            without_password.len()
        ));

        if without_password.iter().any(|doc| !doc.carried_credentials) {
            cli::hint("Export with --include-credentials to carry passwords.");
        }
    }

    cli::success(&format!("Total: {} document(s) imported", imported.len()));
}

/// Read and parse an export, refusing a format newer than this binary
/// understands.
fn read_export_file(file: &Path) -> Result<ExportFile> {
    let content =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;

    let export_file: ExportFile = from_str(&content).context("Failed to parse JSON")?;

    if export_file.format_version > EXPORT_FORMAT_VERSION {
        bail!(
            "This export uses format version {} but this crap-cms only supports up to {}. \
             Upgrade crap-cms to import it.",
            export_file.format_version,
            EXPORT_FORMAT_VERSION
        );
    }

    let current = env!("CARGO_PKG_VERSION");
    if let Some(warning) =
        CrapConfig::check_version_against(Some(&export_file.crap_version), current)
    {
        cli::warning(&warning.replace("config requires", "export file was created with"));
    }

    Ok(export_file)
}

/// The collections to import: the one `collection_filter` names, or every
/// collection in the export.
fn import_slugs(export_file: &ExportFile, collection_filter: Option<&str>) -> Result<Vec<String>> {
    let Some(slug) = collection_filter else {
        return Ok(export_file.collections.keys().cloned().collect());
    };

    if !export_file.collections.contains_key(slug) {
        bail!("Collection '{slug}' not found in import file");
    }

    Ok(vec![slug.to_string()])
}

/// One collection's documents in the export and its target in the schema.
fn resolve_batch<'a>(
    tx: &dyn DbConnection,
    source: &ImportSource<'a>,
    slug: &'a str,
) -> Result<ImportBatch<'a>> {
    let def = source
        .registry
        .get_collection(slug)
        .ok_or_else(|| anyhow!("Collection '{slug}' exists in import file but not in schema"))?;

    let docs = source
        .export_file
        .collections
        .get(slug)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Expected array for collection '{slug}'"))?;

    Ok(ImportBatch {
        target: ImportTarget::resolve(tx, slug, def, source.locale)?,
        docs,
    })
}

/// Import collection data from JSON, all of it in one transaction: an import
/// that fails leaves nothing behind.
///
/// # Errors
///
/// Returns an error if config loading, file reading, JSON parsing, or any
/// per-document write fails.
#[cfg(not(tarpaulin_include))]
pub fn import(config_dir: &Path, file: &Path, collection_filter: Option<&str>) -> Result<()> {
    let Project {
        lock: _instance_lock,
        config: cfg,
        registry,
        pool,
    } = open_project(config_dir)?;

    let export_file = read_export_file(file)?;
    let slugs = import_slugs(&export_file, collection_filter)?;
    check_import_slugs(&registry, &slugs)?;

    let mut conn = pool.write().context("Failed to get database connection")?;
    // IMMEDIATE takes SQLite's write lock up front, so the import's reads and
    // writes on one transaction can't fail on a busy snapshot under a
    // concurrent writer. On Postgres it is a plain transaction.
    let tx = conn
        .transaction_immediate()
        .context("Failed to begin transaction")?;

    let source = ImportSource {
        registry: &registry,
        export_file: &export_file,
        locale: &cfg.locale,
    };
    let batches = slugs
        .iter()
        .map(|slug| resolve_batch(&tx, &source, slug))
        .collect::<Result<Vec<_>>>()?;

    check_duplicate_ids(&batches)?;
    check_totp_secrets(&batches, cfg.auth.secret.as_ref())?;

    let imported = import_batches(&tx, &batches)?;

    tx.commit().context("Failed to commit the import")?;
    report_import(&batches, &imported);

    Ok(())
}

/// Verify every collection in the import set exists in the registry BEFORE
/// any write, so an unknown slug is reported up front rather than midway.
fn check_import_slugs(registry: &Registry, slugs: &[String]) -> Result<()> {
    let unknown: Vec<&str> = slugs
        .iter()
        .filter(|s| registry.get_collection(s).is_none())
        .map(String::as_str)
        .collect();

    if unknown.is_empty() {
        return Ok(());
    }

    bail!(
        "Collection(s) {} exist in the import file but not in the schema — nothing was imported",
        unknown
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::DatabaseConfig,
        core::{
            CollectionDefinition, FieldDefinition, FieldType, auth::seal_totp_secret,
            collection::Auth, field::RelationshipConfig,
        },
        db::{DbPool, DbValue, InMemoryConn, migrate, pool},
    };
    use tempfile::{TempDir, tempdir};

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

    /// Regression: an unknown slug must be rejected before any collection is
    /// written (previously detected lazily, after earlier collections had
    /// already committed).
    #[test]
    fn unknown_import_slugs_rejected_up_front() {
        let shared = Registry::shared();
        shared
            .write()
            .unwrap()
            .register_collection(CollectionDefinition::new("posts"));
        let registry = (*Registry::snapshot(&shared)).clone();

        assert!(check_import_slugs(&registry, &["posts".to_string()]).is_ok());

        let err = check_import_slugs(
            &registry,
            &[
                "posts".to_string(),
                "ghosts".to_string(),
                "zombies".to_string(),
            ],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("'ghosts', 'zombies'"), "{err}");
        assert!(err.contains("nothing was imported"), "{err}");
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

    /// A TOTP secret sealed under another auth secret is refused, naming the
    /// account; one sealed under this installation's secret imports.
    #[test]
    fn totp_secrets_sealed_with_another_auth_secret_are_refused() {
        let def = CollectionDefinition::new("users");
        let locale = LocaleConfig::default();
        let target = || {
            ImportTarget::builder("users", &def, &locale)
                .credential_columns(vec!["_totp_secret"])
                .build()
        };

        let here = seal_totp_secret("this-secret", "JBSWY3DPEHPK3PXP").unwrap();
        let elsewhere = seal_totp_secret("another-secret", "JBSWY3DPEHPK3PXP").unwrap();

        let docs = [
            json!({ "id": "u1", "_credentials": { "_totp_secret": here } }),
            json!({ "id": "u2", "_credentials": { "_totp_secret": elsewhere } }),
        ];

        let refused = [ImportBatch {
            target: target(),
            docs: &docs,
        }];
        let err = check_totp_secrets(&refused, "this-secret")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("users/u2") && !err.contains("users/u1"),
            "{err}"
        );

        let accepted = [ImportBatch {
            target: target(),
            docs: &docs[..1],
        }];
        assert!(check_totp_secrets(&accepted, "this-secret").is_ok());
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

    /// An account of a collection without password login logs in some other
    /// way, so it isn't reported as left without a password.
    #[test]
    fn accounts_without_password_login_lack_no_password() {
        let mut auth = Auth::new(true);
        auth.methods.clear();
        let mut def = CollectionDefinition::new("members");
        def.auth = Some(auth);
        let locale = LocaleConfig::default();
        let target = ImportTarget::builder("members", &def, &locale).build();

        // No table exists: a collection without password login is never queried.
        let conn = InMemoryConn::open();
        assert!(!lacks_password(&conn, &target, "m1").unwrap());
    }

    /// A document listed twice in one collection is refused before anything
    /// is written, instead of the later copy overwriting the earlier one.
    #[test]
    fn duplicate_document_ids_are_refused() {
        let def = CollectionDefinition::new("posts");
        let locale = LocaleConfig::default();
        let docs = vec![
            json!({ "id": "p1" }),
            json!({ "id": "p2" }),
            json!({ "id": "p1" }),
        ];
        let batches = [ImportBatch {
            target: ImportTarget::builder("posts", &def, &locale).build(),
            docs: &docs,
        }];

        let err = check_duplicate_ids(&batches).unwrap_err().to_string();

        assert!(err.contains("posts/p1") && !err.contains("p2"), "{err}");
    }
}
