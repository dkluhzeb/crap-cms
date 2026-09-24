//! Deferred format conversion for the image processing queue.

use crate::core::{
    DocumentFields,
    upload::{CollectionUpload, key_from_served_url, served_url},
};

/// A deferred format conversion to be inserted into the image processing queue.
#[derive(Debug, Clone)]
pub struct QueuedConversion {
    pub source_path: String,
    pub target_path: String,
    pub format: String,
    pub quality: u8,
    pub url_column: String,
    pub url_value: String,
}

impl QueuedConversion {
    /// The conversion that turns one already-stored size file into `format`.
    ///
    /// The variant lives beside its source under the same stem — `size_key`
    /// with its extension swapped for the format name — and fills the
    /// `{size}_{format}_url` column with that file's served url. Deriving all
    /// three from the size key is what lets the upload pipeline (which has just
    /// written the size file) and the publish that adopts a draft's stored file
    /// (which has only the stored `{size}_url`) name the same target.
    #[must_use]
    pub fn for_size(size_key: &str, size_name: &str, format: &str, quality: u8) -> Self {
        let stem = size_key.rsplit_once('.').map_or(size_key, |(stem, _)| stem);
        let target_path = format!("{stem}.{format}");

        Self {
            source_path: size_key.to_string(),
            url_value: served_url(&target_path),
            target_path,
            format: format.to_string(),
            quality,
            url_column: format!("{size_name}_{format}_url"),
        }
    }

    /// Every queued (`queue = true`) conversion the stored file described by
    /// `fields` owes: one per configured deferred format for each generated
    /// size whose `{size}_url` the fields carry.
    ///
    /// Derived from the stored size files rather than remembered anywhere: a
    /// deferred variant is a function of the size file and the collection's
    /// format options, so every write that makes a stored file live again — a
    /// publish adopting a drafted file, a version restore — queues exactly the
    /// jobs the current configuration calls for.
    #[must_use]
    pub fn deferred_for(fields: &DocumentFields, upload: &CollectionUpload) -> Vec<Self> {
        let mut queued = Vec::new();

        for size in &upload.image_sizes {
            let Some(size_key) = fields
                .get_str(&format!("{}_url", size.name))
                .and_then(key_from_served_url)
            else {
                continue;
            };

            for (format, opts) in upload.format_options.deferred() {
                queued.push(Self::for_size(size_key, &size.name, format, opts.quality));
            }
        }

        queued
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::upload::{FormatQuality, ImageSizeBuilder};

    fn upload(queue_webp: bool) -> CollectionUpload {
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumbnail")
                .width(300)
                .height(300)
                .build(),
            ImageSizeBuilder::new("card").width(600).height(400).build(),
        ];
        upload.format_options.webp = Some(FormatQuality::new(80, queue_webp));
        upload.format_options.avif = Some(FormatQuality::new(50, false));
        upload
    }

    /// One queued job per deferred format for each size the fields carry — a
    /// size the file never produced (no `{size}_url`) owes nothing, and a
    /// format converted synchronously owes nothing.
    #[test]
    fn deferred_for_queues_each_carried_size_once_per_deferred_format() {
        let fields: DocumentFields = [(
            "thumbnail_url".to_string(),
            json!("/uploads/media/abc_photo_thumbnail.png"),
        )]
        .into_iter()
        .collect();

        let queued = QueuedConversion::deferred_for(&fields, &upload(true));

        assert_eq!(queued.len(), 1, "{queued:?}");
        assert_eq!(queued[0].source_path, "media/abc_photo_thumbnail.png");
        assert_eq!(queued[0].url_column, "thumbnail_webp_url");

        assert!(QueuedConversion::deferred_for(&fields, &upload(false)).is_empty());
    }

    /// The variant sits beside its source with the format as its extension, and
    /// names the column and url the conversion job fills.
    #[test]
    fn for_size_swaps_the_extension_and_names_the_column() {
        let c =
            QueuedConversion::for_size("media/abc_photo_thumbnail.png", "thumbnail", "webp", 80);

        assert_eq!(c.source_path, "media/abc_photo_thumbnail.png");
        assert_eq!(c.target_path, "media/abc_photo_thumbnail.webp");
        assert_eq!(c.url_value, "/uploads/media/abc_photo_thumbnail.webp");
        assert_eq!(c.url_column, "thumbnail_webp_url");
        assert_eq!(c.quality, 80);
    }

    /// Only the LAST dot separates the extension — a stem carrying dots keeps
    /// them, so the variant stays beside the file it converts.
    #[test]
    fn for_size_only_replaces_the_final_extension() {
        let c = QueuedConversion::for_size("media/my.photo.v2_card.png", "card", "avif", 50);

        assert_eq!(c.target_path, "media/my.photo.v2_card.avif");
    }

    /// An extension-less key gets the format appended rather than losing part
    /// of its name.
    #[test]
    fn for_size_appends_to_a_key_without_an_extension() {
        let c = QueuedConversion::for_size("media/plain_thumb", "thumb", "webp", 80);

        assert_eq!(c.target_path, "media/plain_thumb.webp");
    }
}
