//! Deferred format conversion for the image processing queue.

use crate::core::upload::served_url;

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
