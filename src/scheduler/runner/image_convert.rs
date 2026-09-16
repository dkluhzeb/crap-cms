//! The `_system_image_convert` job: encode, write the URL column, complete,
//! and report the change.

use std::time::Instant;

use anyhow::{Context as _, Result};
use serde_json::from_str;
use tracing::{info, warn};

use crate::{
    core::{
        JobRun,
        event::EventOperation,
        upload::{self, ImageConvertJobData, SharedStorage, served_url},
    },
    db::{
        DbConnection, DbPool, DbValue, LocaleContext, query,
        query::{helpers::utc_now, jobs as job_query},
    },
    scheduler::runner::failure::{record_job_failure, record_permanent_job_failure},
    service::{AppInfra, ServiceContext, helpers::shape_reported},
};

/// Borrowed inputs of an image-convert job run.
pub(super) struct ImageConvertRun<'a> {
    pub(super) pool: &'a DbPool,
    pub(super) job_run: &'a JobRun,
    pub(super) storage: &'a SharedStorage,
    /// The infra to report the finished conversion through; `None` in contexts
    /// that publish no events.
    pub(super) app_infra: Option<&'a AppInfra>,
}

/// Execute a `_system_image_convert` job: encode the source image,
/// write the converted bytes to storage, update the target document's
/// URL column, and mark the job completed. On encode / storage / DB
/// failure, defer to the job runner's standard `fail_job` retry path.
///
/// Mirrors the shape of
/// [`execute_system_email`](crate::scheduler::runner::execute::execute_system_email)
/// — no Lua VM, no outer transaction held during the slow encode step.
pub(super) fn execute_system_image_convert(
    run: &ImageConvertRun<'_>,
    start: Instant,
) -> Result<()> {
    let &ImageConvertRun {
        pool,
        job_run,
        storage,
        app_infra,
    } = run;

    let data: ImageConvertJobData =
        from_str(&job_run.data).context("Invalid image-convert job data")?;

    let label = format!("Image-convert job {}", job_run.id);

    if !query::is_valid_identifier(&data.collection) {
        let error_msg = format!("invalid collection slug: {}", data.collection);
        record_permanent_job_failure(pool, job_run, &label, &error_msg)?;
        return Ok(());
    }
    if !query::is_valid_identifier(&data.url_column) {
        let error_msg = format!("invalid url_column: {}", data.url_column);
        record_permanent_job_failure(pool, job_run, &label, &error_msg)?;
        return Ok(());
    }

    let encode_result = upload::process_image_entry_with_storage(
        &data.source_path,
        &data.target_path,
        &data.format,
        data.quality,
        &**storage,
    );

    match encode_result {
        Ok(()) => {
            let mut conn = pool
                .get()
                .context("Failed to get DB connection for image-convert completion")?;

            // One IMMEDIATE tx wraps the URL write + completion mark so the
            // queue row never lands in `completed` while the document's URL
            // column is unchanged, or vice versa. Same atomicity property
            // the legacy `record_conversion_success` provided.
            let tx = conn
                .transaction_immediate()
                .context("Failed to begin image-convert completion transaction")?;

            let timestamps = app_infra
                .and_then(|infra| infra.registry.get_collection(&data.collection))
                .is_some_and(|def| def.timestamps);
            write_converted_url(&tx, &data, timestamps, storage)?;

            job_query::complete_job(&tx, &job_run.id, job_run.attempt, None)?;

            tx.commit()
                .context("Failed to commit image-convert completion transaction")?;

            if let Some(infra) = app_infra {
                report_conversion(infra, &data);
            }

            info!(
                "Image-convert job {} completed in {:?} ({} → {})",
                job_run.id,
                start.elapsed(),
                data.format,
                data.target_path
            );
        }
        Err(e) => {
            record_job_failure(pool, job_run, &label, &e)?;
        }
    }

    Ok(())
}

/// The `SET` clauses of the completion write and their parameters. A
/// collection with timestamps also gets `updated_at`, so the document reads as
/// changed.
fn converted_url_sets(
    conn: &dyn DbConnection,
    data: &ImageConvertJobData,
    timestamps: bool,
) -> (Vec<String>, Vec<DbValue>) {
    let mut sets = vec![format!("\"{}\" = {}", data.url_column, conn.placeholder(1))];
    let mut params = vec![DbValue::Text(data.url_value.clone())];

    if timestamps {
        sets.push(format!("updated_at = {}", conn.placeholder(2)));
        params.push(DbValue::Text(utc_now()));
    }

    (sets, params)
}

/// The document column holding this conversion's source image URL, paired with
/// the value it must still hold for the conversion to describe the document as
/// it now stands.
///
/// A conversion already running when the document's file is replaced finishes
/// against the new row, and without this would write the *old* file's
/// derivative URL over the new file's. Every size image is named after the
/// upload's unique stem, so a replacement rewrites the size column — the row
/// still pointing at this job's source is exactly what makes the write current.
///
/// `None` for a payload whose `url_column` is not the `{size}_{format}_url`
/// the upload pipeline produces (a job queued by an older format): such a job
/// keeps the unguarded write it was enqueued with rather than never landing.
fn source_url_guard(data: &ImageConvertJobData) -> Option<(String, String)> {
    let size = data
        .url_column
        .strip_suffix(&format!("_{}_url", data.format))?;

    let column = format!("{size}_url");
    if !query::is_valid_identifier(&column) {
        return None;
    }

    Some((column, served_url(&data.source_path)))
}

/// Write a finished conversion's URL to the document's column — and, for a
/// collection with timestamps, `updated_at`, so the document reads as changed.
///
/// The write applies only while the document still references this job's
/// source image. Two things break that: a purge removes only *pending* and
/// *failed* conversions, so one already running finishes against a row that is
/// gone; and a replacement upload rewrites the size columns, so a conversion
/// of the previous file finishes against a row it no longer describes. Either
/// way the UPDATE matches nothing and the derivative just written would sit in
/// storage forever with nothing referencing it — so it is deleted here.
fn write_converted_url(
    conn: &dyn DbConnection,
    data: &ImageConvertJobData,
    timestamps: bool,
    storage: &SharedStorage,
) -> Result<()> {
    let (sets, mut params) = converted_url_sets(conn, data, timestamps);

    let mut wheres = vec![format!("id = {}", conn.placeholder(params.len() + 1))];
    params.push(DbValue::Text(data.document_id.clone()));

    if let Some((column, expected)) = source_url_guard(data) {
        wheres.push(format!(
            "\"{column}\" = {}",
            conn.placeholder(params.len() + 1)
        ));
        params.push(DbValue::Text(expected));
    }

    let updated = conn
        .execute(
            &format!(
                "UPDATE \"{}\" SET {} WHERE {}",
                data.collection,
                sets.join(", "),
                wheres.join(" AND ")
            ),
            &params,
        )
        .context("Failed to update document URL column")?;

    if updated == 0 {
        discard_orphaned_derivative(data, storage);
    }

    Ok(())
}

/// Remove a derivative nothing will reference — its document is gone, or the
/// document's file has since been replaced so the row no longer points at this
/// conversion's source. Best-effort: the object is already unreferenced, so a
/// failed delete is logged and the job still completes rather than retrying a
/// conversion that can never land.
fn discard_orphaned_derivative(data: &ImageConvertJobData, storage: &SharedStorage) {
    info!(
        "Image-convert for {}/{} no longer applies — discarding {}",
        data.collection, data.document_id, data.target_path
    );

    let _ = storage.delete(&data.target_path).inspect_err(|e| {
        warn!(
            "Failed to delete orphaned derivative {}: {e:#}",
            data.target_path
        );
    });
}

/// Tell readers a finished conversion changed the document, as any write does:
/// invalidate the populate cache and publish an update event carrying the
/// document in the shape a read returns. A system write — no hooks, no version
/// snapshot, and no strip: delivery strips per subscriber.
///
/// The read excludes trashed rows, so a conversion finishing on a trashed
/// upload publishes nothing; its URL is on the row for when it is restored.
fn report_conversion(infra: &AppInfra, data: &ImageConvertJobData) {
    let Some(def) = infra.registry.get_collection(&data.collection) else {
        return;
    };

    let ctx = ServiceContext::collection(&data.collection, def)
        .infra(infra)
        .build();
    ctx.clear_cache();

    let locale_ctx = LocaleContext::default_for(&infra.locale_config);
    let doc = infra
        .pool
        .get()
        .and_then(|conn| {
            query::find_by_id(
                &conn,
                &data.collection,
                def,
                &data.document_id,
                locale_ctx.as_ref(),
            )
        })
        .inspect_err(|e| {
            warn!(
                "Image-convert event for {}/{}: {e:#}",
                data.collection, data.document_id
            );
        })
        .ok()
        .flatten();

    let Some(mut doc) = doc else {
        return;
    };

    shape_reported(&ctx, &mut doc);
    ctx.publish_mutation_event(EventOperation::Update, &doc.id, &doc.fields);
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::collections::HashMap;

    use rusqlite::Connection;
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        admin::test_support::test_infra_with_events,
        config::UploadConfig,
        core::{
            CollectionDefinition, FieldDefinition, FieldType, LiveMode,
            upload::{CollectionUpload, ImageSize, create_storage},
        },
        scheduler::runner::test_support::convert_job,
    };

    /// Local storage in a temp dir holding the conversion's target object.
    fn storage_with_derivative(dir: &tempfile::TempDir) -> SharedStorage {
        let storage = create_storage(dir.path(), &UploadConfig::default()).unwrap();
        storage.put("a.webp", b"converted", "image/webp").unwrap();

        storage
    }

    /// Regression: a finished image conversion wrote its URL without touching
    /// `updated_at`, so the document looked unchanged to readers and caches.
    /// A media table shaped like an upload collection with one image size:
    /// the size's own URL column plus the format derivative's.
    fn media_table(rows: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE media (
                 id TEXT PRIMARY KEY,
                 thumbnail_url TEXT,
                 thumbnail_webp_url TEXT,
                 updated_at TEXT
             );
             {rows}"
        ))
        .unwrap();

        conn
    }

    #[test]
    fn a_converted_url_bumps_updated_at() {
        let conn = media_table(
            "INSERT INTO media (id, thumbnail_url, updated_at) \
             VALUES ('m1', '/uploads/a.png', '2000-01-01T00:00:00.000Z');",
        );
        let data = convert_job("media", "m1");
        let tmp = tempfile::tempdir().unwrap();
        let storage = storage_with_derivative(&tmp);

        write_converted_url(&conn, &data, true, &storage).unwrap();

        assert!(
            storage.exists("a.webp").unwrap(),
            "a derivative the document references must be kept"
        );

        let row = DbConnection::query_one(
            &conn,
            "SELECT thumbnail_webp_url, updated_at FROM media WHERE id = 'm1'",
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            row.get_string("thumbnail_webp_url").unwrap(),
            "/uploads/a.webp"
        );
        assert_ne!(
            row.get_string("updated_at").unwrap(),
            "2000-01-01T00:00:00.000Z"
        );
    }

    /// Regression: a purge deletes only *pending* and *failed* conversions, so
    /// one already running finishes after its document row is gone. The UPDATE
    /// then matched nothing and the derivative it had just written stayed in
    /// storage forever, referenced by nothing.
    #[test]
    fn a_conversion_whose_document_is_gone_discards_its_derivative() {
        let conn = media_table("");
        let tmp = tempfile::tempdir().unwrap();
        let storage = storage_with_derivative(&tmp);

        write_converted_url(&conn, &convert_job("media", "gone"), false, &storage).unwrap();

        assert!(
            DbConnection::query_one(&conn, "SELECT id FROM media", &[])
                .unwrap()
                .is_none(),
            "nothing may be written for a document that no longer exists"
        );
        assert!(
            !storage.exists("a.webp").unwrap(),
            "the orphaned derivative must be removed from storage"
        );
    }

    /// Regression: a conversion already running when the document's file was
    /// replaced finished against the new row and wrote the *old* file's
    /// derivative URL over the new file's — leaving the document serving a
    /// derivative of an image it no longer holds. The write now applies only
    /// while the row still points at the source this job converted.
    #[test]
    fn a_conversion_of_a_replaced_file_writes_nothing_and_discards_its_derivative() {
        // `m1`'s thumbnail is `b.png` now; the running job converted `a.png`.
        let conn = media_table(
            "INSERT INTO media (id, thumbnail_url, thumbnail_webp_url) \
             VALUES ('m1', '/uploads/b.png', '/uploads/b.webp');",
        );
        let tmp = tempfile::tempdir().unwrap();
        let storage = storage_with_derivative(&tmp);

        write_converted_url(&conn, &convert_job("media", "m1"), false, &storage).unwrap();

        let row = DbConnection::query_one(
            &conn,
            "SELECT thumbnail_webp_url FROM media WHERE id = 'm1'",
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            row.get_string("thumbnail_webp_url").unwrap(),
            "/uploads/b.webp",
            "the current file's derivative must not be overwritten"
        );
        assert!(
            !storage.exists("a.webp").unwrap(),
            "the superseded derivative must be removed from storage"
        );
    }

    /// The guard names the size's own URL column, derived from the
    /// `{size}_{format}_url` column the upload pipeline writes — and gives up
    /// on a payload that does not follow that shape.
    #[test]
    fn the_guard_names_the_size_column_of_a_pipeline_payload() {
        assert_eq!(
            source_url_guard(&convert_job("media", "m1")),
            Some(("thumbnail_url".to_string(), "/uploads/a.png".to_string()))
        );

        let mut older = convert_job("media", "m1");
        older.url_column = "sizes__medium__formats__webp__url".into();
        assert_eq!(source_url_guard(&older), None);
    }

    /// A job queued by an older format carries no size column to match on, so
    /// it keeps the unguarded write rather than never landing.
    #[test]
    fn a_payload_without_a_size_column_keeps_the_unguarded_write() {
        let conn = media_table(
            "INSERT INTO media (id, thumbnail_url) VALUES ('m1', '/uploads/elsewhere.png');",
        );
        let tmp = tempfile::tempdir().unwrap();
        let storage = storage_with_derivative(&tmp);

        // `thumbnail_webp_url` does not end in `_avif_url`, so no size column
        // can be derived from it.
        let mut older = convert_job("media", "m1");
        older.format = "avif".into();

        write_converted_url(&conn, &older, false, &storage).unwrap();

        let row = DbConnection::query_one(
            &conn,
            "SELECT thumbnail_webp_url FROM media WHERE id = 'm1'",
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            row.get_string("thumbnail_webp_url").unwrap(),
            "/uploads/a.webp"
        );
        assert!(storage.exists("a.webp").unwrap());
    }

    /// An upload collection with one image size and an array field, live in
    /// full mode, holding `m1` with a stored thumbnail and one array row.
    fn media_with_sizes_and_rows() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        def.live_mode = LiveMode::Full;
        def.upload = Some(CollectionUpload {
            enabled: true,
            image_sizes: vec![ImageSize::builder("thumbnail").width(10).height(10).build()],
            ..Default::default()
        });
        def.fields = vec![
            FieldDefinition::builder("thumbnail_url", FieldType::Text).build(),
            FieldDefinition::builder("thumbnail_width", FieldType::Number).build(),
            FieldDefinition::builder("thumbnail_height", FieldType::Number).build(),
            FieldDefinition::builder("shots", FieldType::Array)
                .fields(shot_fields())
                .build(),
        ];

        def
    }

    fn shot_fields() -> Vec<FieldDefinition> {
        vec![FieldDefinition::builder("caption", FieldType::Text).build()]
    }

    /// Seed `m1` with a stored thumbnail and one `shots` row.
    fn seed_media_row(pool: &DbPool) {
        let conn = pool.get().unwrap();
        conn.execute(
            "INSERT INTO media (id, thumbnail_url, thumbnail_width, thumbnail_height) \
             VALUES ('m1', '/uploads/t.png', 10, 10)",
            &[],
        )
        .unwrap();

        let rows = [HashMap::from([("caption".to_string(), json!("first"))])];
        query::set_array_rows(&conn, "media", "shots", "m1", &rows, &shot_fields(), None).unwrap();
    }

    /// Regression: a finished conversion published the raw stored row — the
    /// per-size values flat instead of folded into `sizes`, and no array rows —
    /// where every other write reports the shape a read returns.
    #[test]
    fn a_finished_conversion_is_reported_once_in_the_read_shape() {
        let (_tmp, infra, mut rx) = test_infra_with_events(media_with_sizes_and_rows());
        seed_media_row(&infra.pool);
        infra.cache.set("populate:media:m1", b"stale").unwrap();

        report_conversion(&infra, &convert_job("media", "m1"));

        assert!(
            !infra.cache.has("populate:media:m1").unwrap(),
            "the populate cache must be cleared"
        );

        let event = rx.try_recv().expect("an update event");
        assert!(matches!(event.operation, EventOperation::Update));
        assert!(rx.try_recv().is_err(), "exactly one event");

        assert!(event.data.contains_key("sizes"), "{:?}", event.data);
        assert!(
            !event.data.contains_key("thumbnail_url"),
            "{:?}",
            event.data
        );
        assert_eq!(
            event
                .data
                .get("shots")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1),
            "the array rows must be hydrated: {:?}",
            event.data
        );
    }
}
