//! The file half of a version restore: the queued image variants of the file it
//! makes live — named again when the document still holds them, owed as jobs
//! when it does not.

use serde_json::Value;

use crate::{
    core::{
        CollectionDefinition, DocumentFields,
        upload::{QueuedConversion, key_from_served_url},
    },
    service::{UploadConversions, write::upload_files::DocumentFiles},
};

/// Whether `fields` leaves `column` without a url.
fn column_empty(fields: &DocumentFields, column: &str) -> bool {
    fields.get_str(column).is_none_or(str::is_empty)
}

/// Name again, in the snapshot a restore writes, every queued variant of its
/// file whose bytes the document still holds.
///
/// A queued format variant is never part of a write, so a snapshot taken when
/// its file was stored records that variant's column empty — and the job that
/// later filled the row never touches the snapshot. When the document still
/// references the variant's key (its live row, or another snapshot), the bytes
/// are that variant: the key is derived from the very size file the snapshot
/// names. Naming them again keeps them referenced. Re-queueing the job
/// instead left them referenced by nothing until it ran — and a job cancelled
/// before it runs (the file replaced again, the document deleted) left them in
/// storage for good.
pub(crate) fn adopt_held_variants(
    def: &CollectionDefinition,
    before: &DocumentFiles,
    snapshot: &mut Value,
) {
    let Some(upload) = def.upload.as_ref().filter(|u| u.enabled) else {
        return;
    };

    let Some(map) = snapshot.as_object_mut() else {
        return;
    };

    let fields: DocumentFields = map.clone().into_iter().collect();

    for variant in QueuedConversion::deferred_for(&fields, upload) {
        if !column_empty(&fields, &variant.url_column) || !before.names(&variant.target_path) {
            continue;
        }

        map.insert(variant.url_column, Value::String(variant.url_value));
    }
}

/// The conversions a version restore owes the file it makes live, or `None`
/// when it owes none and changed no file.
///
/// A variant [`adopt_held_variants`] could not name again — the document no
/// longer holds its bytes — would otherwise leave its column empty for good.
/// So every deferred conversion whose column the restored row leaves empty is
/// queued again, derived from the restored size files exactly as a publish of
/// a drafted file derives them.
///
/// A restore that swaps the published file owes a settle plan even when
/// nothing is left to queue: the previous file's still-queued jobs must be
/// cancelled, or they land after the restore and write a derivative of a file
/// the row no longer names.
pub(crate) fn restored_file_conversions(
    def: &CollectionDefinition,
    before: &DocumentFiles,
    restored: &DocumentFields,
    max_attempts: u32,
) -> Option<UploadConversions> {
    let upload = def.upload.as_ref().filter(|u| u.enabled)?;

    let owed: Vec<QueuedConversion> = QueuedConversion::deferred_for(restored, upload)
        .into_iter()
        .filter(|c| column_empty(restored, &c.url_column))
        .collect();

    let file_changed = match restored.get_str("url").and_then(key_from_served_url) {
        Some(key) => !before.live.iter().any(|live| live == key),
        None => !before.live.is_empty(),
    };

    if owed.is_empty() && !file_changed {
        return None;
    }

    Some(UploadConversions::new(owed, max_attempts))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::upload::{CollectionUpload, FormatQuality, ImageSizeBuilder};

    /// The files a published row names, nothing held by a snapshot.
    fn live_only(keys: &[&str]) -> DocumentFiles {
        DocumentFiles::new(keys.iter().map(ToString::to_string).collect(), Vec::new())
    }

    /// `media` with a `thumbnail` size whose webp variant is converted on the
    /// queue.
    fn media_with_queued_webp() -> CollectionDefinition {
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumbnail")
                .width(300)
                .height(300)
                .build(),
        ];
        upload.format_options.webp = Some(FormatQuality::new(80, true));

        let mut def = CollectionDefinition::new("media");
        def.upload = Some(upload);

        def
    }

    /// A restored row: the original, its thumbnail, and optionally the queued
    /// webp column.
    fn restored_row(original: &str, webp: Option<&str>) -> DocumentFields {
        let stem = original.trim_end_matches(".png");

        let mut fields: DocumentFields = [
            (
                "url".to_string(),
                json!(format!("/uploads/media/{original}")),
            ),
            (
                "thumbnail_url".to_string(),
                json!(format!("/uploads/media/{stem}_thumbnail.png")),
            ),
        ]
        .into_iter()
        .collect();

        fields.insert(
            "thumbnail_webp_url".to_string(),
            webp.map_or(Value::Null, |w| json!(w)),
        );

        fields
    }

    /// Regression: a snapshot records a queued variant's column empty (the
    /// job fills the row, never the snapshot), and restoring it queued
    /// nothing — the variant was gone for good. The restore now owes the job
    /// again.
    #[test]
    fn a_restore_requeues_the_variant_its_snapshot_left_empty() {
        let def = media_with_queued_webp();
        let before = live_only(&[
            "media/a.png",
            "media/a_thumbnail.png",
            "media/a_thumbnail.webp",
        ]);

        let owed = restored_file_conversions(&def, &before, &restored_row("a.png", None), 3)
            .expect("the empty queued column is owed");

        assert_eq!(owed.queued.len(), 1, "{:?}", owed.queued);
        assert_eq!(owed.queued[0].source_path, "media/a_thumbnail.png");
        assert_eq!(owed.queued[0].url_column, "thumbnail_webp_url");
        assert_eq!(owed.max_attempts, 3);
    }

    /// The snapshot's side of a restore: `url`, the thumbnail, and the queued
    /// webp column empty, as the snapshot taken at upload recorded it.
    fn snapshot_without_variant(original: &str) -> Value {
        let stem = original.trim_end_matches(".png");

        json!({
            "url": format!("/uploads/media/{original}"),
            "thumbnail_url": format!("/uploads/media/{stem}_thumbnail.png"),
            "thumbnail_webp_url": null,
        })
    }

    /// Regression: restoring the live file re-queued a variant whose bytes the
    /// row still named, leaving them referenced by nothing until the job ran —
    /// forever, if the job was cancelled first. A variant the document still
    /// holds is named again instead, and nothing is owed.
    #[test]
    fn a_restore_names_the_variant_the_document_still_holds() {
        let def = media_with_queued_webp();
        let before = live_only(&[
            "media/a.png",
            "media/a_thumbnail.png",
            "media/a_thumbnail.webp",
        ]);

        let mut snapshot = snapshot_without_variant("a.png");
        adopt_held_variants(&def, &before, &mut snapshot);

        assert_eq!(
            snapshot["thumbnail_webp_url"],
            json!("/uploads/media/a_thumbnail.webp")
        );

        let restored: DocumentFields = snapshot
            .as_object()
            .expect("an object")
            .clone()
            .into_iter()
            .collect();
        assert!(restored_file_conversions(&def, &before, &restored, 3).is_none());
    }

    /// A variant held only by an older snapshot is named again too — its bytes
    /// are stored as long as that snapshot names them.
    #[test]
    fn a_variant_held_by_a_snapshot_is_named_again() {
        let def = media_with_queued_webp();
        let before = DocumentFiles::new(
            vec!["media/b.png".to_string()],
            vec!["media/a_thumbnail.webp".to_string()],
        );

        let mut snapshot = snapshot_without_variant("a.png");
        adopt_held_variants(&def, &before, &mut snapshot);

        assert_eq!(
            snapshot["thumbnail_webp_url"],
            json!("/uploads/media/a_thumbnail.webp")
        );
    }

    /// A variant the document no longer holds stays empty for the job to fill.
    #[test]
    fn a_variant_the_document_no_longer_holds_is_left_to_the_job() {
        let def = media_with_queued_webp();
        let before = live_only(&["media/b.png", "media/b_thumbnail.png"]);

        let mut snapshot = snapshot_without_variant("a.png");
        adopt_held_variants(&def, &before, &mut snapshot);

        assert_eq!(snapshot["thumbnail_webp_url"], Value::Null);
    }

    /// A restored row whose variant column is filled owes nothing — and, the
    /// file being the live one, needs no settle plan at all.
    #[test]
    fn a_restore_of_a_complete_row_of_the_live_file_owes_nothing() {
        let def = media_with_queued_webp();
        let before = live_only(&["media/a.png", "media/a_thumbnail.png"]);
        let row = restored_row("a.png", Some("/uploads/media/a_thumbnail.webp"));

        assert!(restored_file_conversions(&def, &before, &row, 3).is_none());
    }

    /// Swapping the published file owes a settle plan even with nothing to
    /// queue, so the previous file's pending jobs are cancelled.
    #[test]
    fn a_restore_that_swaps_the_file_always_settles() {
        let def = media_with_queued_webp();
        let before = live_only(&["media/b.png", "media/b_thumbnail.png"]);
        let row = restored_row("a.png", Some("/uploads/media/a_thumbnail.webp"));

        let owed = restored_file_conversions(&def, &before, &row, 3).expect("a settle plan");
        assert!(owed.queued.is_empty());
    }
}
