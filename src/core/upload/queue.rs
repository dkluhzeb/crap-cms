//! Queued image format conversion via the job system.
//!
//! `queue_image_conversion()` inserts a `_system_image_convert` job
//! that the scheduler processes with the same retry / heartbeat /
//! recovery story as every other job — replacing the bespoke
//! `_crap_image_queue` table and its single-flight worker.
//!
//! The Rust handler lives in `scheduler/runner.rs::execute_system_image_convert`,
//! mirroring `execute_system_email`.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::db::{DbConnection, query};

/// System job slug for queued image format conversions.
pub const SYSTEM_IMAGE_CONVERT_JOB: &str = "_system_image_convert";

/// Default queue name for system image-conversion jobs. Kept separate
/// from `"default"` so operators can give image work its own
/// concurrency knob (`job_concurrency` map) without touching user
/// jobs.
pub const IMAGE_CONVERT_QUEUE: &str = "images";

/// Fallback `max_attempts` for image conversions when the caller
/// doesn't supply one (e.g. the legacy-queue drain migration).
/// Matches the framework default applied by
/// `JobsConfig::apply_queue_defaults` for `[jobs.queues.images]
/// retries = 2` (= 3 total attempts).
pub const FALLBACK_MAX_ATTEMPTS: u32 = 3;

/// Data payload for a queued image-conversion job. Mirrors the
/// shape of the legacy `NewImageEntry` but as an owned, JSON-friendly
/// struct.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ImageConvertJobData {
    /// Collection slug of the parent upload document.
    pub collection: String,
    /// Parent document ID — used to write the URL column on success.
    pub document_id: String,
    /// Storage key of the source image (original upload).
    pub source_path: String,
    /// Storage key the converted image should be written to.
    pub target_path: String,
    /// Output format: `"webp"` or `"avif"`.
    pub format: String,
    /// Encoder quality (0–100).
    pub quality: u8,
    /// Document column to write with `url_value` on success
    /// (e.g. `"sizes__medium__formats__webp__url"`).
    pub url_column: String,
    /// URL string to set on the column after conversion succeeds
    /// (relative path the admin/API will serve).
    pub url_value: String,
}

/// Queue an image conversion for async processing by the job system.
///
/// `max_attempts` is the total number of attempts (including the
/// initial one). Callers should compute this from the operator's
/// `[jobs.queues.images] retries` setting via
/// `JobsConfig::system_image_max_attempts`. Pass
/// [`FALLBACK_MAX_ATTEMPTS`] when no config is reachable
/// (e.g. one-off migrations).
///
/// Returns the job-run ID.
///
/// # Errors
///
/// Returns an error if serializing the payload or inserting the job
/// row fails.
pub fn queue_image_conversion(
    conn: &dyn DbConnection,
    data: &ImageConvertJobData,
    max_attempts: u32,
) -> Result<String> {
    let data_json = serde_json::to_string(data)?;

    let job = query::jobs::insert_job(
        conn,
        SYSTEM_IMAGE_CONVERT_JOB,
        &data_json,
        "system",
        max_attempts,
        IMAGE_CONVERT_QUEUE,
        0,
    )?;

    tracing::debug!(
        "Queued image conversion for {}/{} ({} → {}) as job {}",
        data.collection,
        data.document_id,
        data.format,
        data.target_path,
        job.id
    );

    Ok(job.id)
}

/// Remove any pending / failed `_system_image_convert` jobs targeting
/// the given document. Called on document delete so a doc removed
/// while conversions are still queued doesn't leave behind orphan
/// jobs that would later fail with "source file not found." Best-effort:
/// returns Ok(()) even when nothing matches.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn delete_image_jobs_for_document(
    conn: &dyn DbConnection,
    collection: &str,
    document_id: &str,
) -> Result<()> {
    // Payload format mirrors `ImageConvertJobData` — match by
    // `"collection":"<slug>"` and `"document_id":"<id>"` JSON substrings.
    // Cheaper than parsing JSON in SQL on every delete; entries that
    // somehow survive will fail at process-time and `images purge`
    // collects them.
    let patterns = vec![
        format!("%\"collection\":\"{collection}\"%"),
        format!("%\"document_id\":\"{document_id}\"%"),
    ];

    crate::db::query::jobs::delete_pending_failed_jobs_matching(
        conn,
        SYSTEM_IMAGE_CONVERT_JOB,
        &patterns,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_convert_job_data_roundtrip() {
        let data = ImageConvertJobData {
            collection: "media".into(),
            document_id: "doc123".into(),
            source_path: "uploads/media/photo.jpg".into(),
            target_path: "uploads/media/photo_medium.webp".into(),
            format: "webp".into(),
            quality: 80,
            url_column: "sizes__medium__formats__webp__url".into(),
            url_value: "/uploads/media/photo_medium.webp".into(),
        };
        let json = serde_json::to_string(&data).expect("serialize");
        let parsed: ImageConvertJobData = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.collection, "media");
        assert_eq!(parsed.format, "webp");
        assert_eq!(parsed.quality, 80);
    }
}
