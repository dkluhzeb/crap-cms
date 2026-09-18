//! The file half of publishing a pending draft: which drafted file goes live,
//! which server-derived columns the request keeps for itself, and the settle
//! plan (queued conversions) the drafted file brings with it.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::{
    core::{
        CollectionDefinition, DocumentFields,
        upload::{CollectionUpload, QueuedConversion, key_from_served_url},
    },
    db::query,
    service::{ServiceContext, UploadConversions, WriteInput, write::stored_row},
};

use super::Result;

/// The document whose drafted file a publish is about to make live. Every
/// field is required and it is built at the one call site, so a plain literal
/// stands in for a builder.
#[derive(Clone, Copy)]
pub(super) struct FilePublish<'a> {
    pub(super) def: &'a CollectionDefinition,
    pub(super) id: &'a str,
    pub(super) upload: &'a CollectionUpload,
}

/// The upload configuration whose drafted file this publish makes live, if any.
///
/// A write that carries `url` processed a file of its own (the multipart
/// handlers inject the derived columns before the write): that file is the one
/// the caller asked for, so it wins and none is adopted. Decided against the
/// request's own data, before the draft's fields are merged in.
///
/// A non-default-locale publish makes the drafted file live like any other:
/// the pending draft is one unit, and the file columns reach the row through
/// the snapshot write-back that carries every shared value. See
/// [`adopt_drafted_file`] for the half of the adoption a translation gets.
pub(super) fn drafted_file_target<'a>(
    def: &'a CollectionDefinition,
    input: &WriteInput<'_>,
) -> Option<&'a CollectionUpload> {
    if !def.is_upload_collection() || input.data.contains_key("url") {
        return None;
    }

    def.upload.as_ref()
}

/// The server-derived upload columns a publish must NOT take from the draft.
///
/// A request that processed a file of its own brings the complete derived set
/// for THAT file — including, as explicit clears, the columns the new file does
/// not produce. Filling any of them from the draft would leave the row
/// describing two files at once: the request's `url` beside the drafted image's
/// `width`, `height` and per-size urls. Empty for every other write, which is
/// what keeps a publish that carries no file adopting the drafted file whole.
pub(super) fn excluded_columns(
    def: &CollectionDefinition,
    input: &WriteInput<'_>,
) -> HashSet<String> {
    if !input.data.contains_key("url") {
        return HashSet::new();
    }

    def.upload
        .as_ref()
        .filter(|u| u.enabled)
        .map(CollectionUpload::derived_field_names)
        .unwrap_or_default()
}

/// The server-derived upload columns `snapshot` carries.
///
/// Sorted, so the write applies them in a stable order regardless of how the
/// derived-name set iterates.
fn drafted_metadata(snapshot: &Map<String, Value>, upload: &CollectionUpload) -> DocumentFields {
    let mut names: Vec<String> = upload.derived_field_names().into_iter().collect();
    names.sort();

    names
        .into_iter()
        .filter_map(|name| {
            let value = snapshot.get(&name)?.clone();

            Some((name, value))
        })
        .collect()
}

/// The conversions the drafted file still owes.
///
/// Re-derived from the size columns the metadata carries rather than stored
/// with the draft: a deferred variant is a function of the stored size file and
/// the collection's format options, and deriving it here means the publish
/// queues exactly the jobs the current configuration calls for.
fn deferred_conversions(
    metadata: &DocumentFields,
    upload: &CollectionUpload,
) -> Vec<QueuedConversion> {
    let mut queued = Vec::new();

    for size in &upload.image_sizes {
        let Some(size_key) = metadata
            .get_str(&format!("{}_url", size.name))
            .and_then(key_from_served_url)
        else {
            continue;
        };

        for (format, opts) in upload.format_options.deferred() {
            queued.push(QueuedConversion::for_size(
                size_key,
                &size.name,
                format,
                opts.quality,
            ));
        }
    }

    queued
}

/// Carry the pending draft's file over to the published row.
///
/// Adds the drafted server-derived columns to `input.data` and attaches the
/// conversions that file still owes, which makes the write settle them: the
/// previous file's still-queued variants are cancelled and the new ones queued,
/// exactly as a write that carried the file itself would.
///
/// Nothing happens when the draft did not change the file — re-queueing there
/// would cancel the conversions still pending for the very file the row keeps.
///
/// # Errors
///
/// Returns a backend error if the published row cannot be read. It must
/// propagate: publishing the old file while reporting success is silent data
/// loss.
pub(super) fn adopt_drafted_file(
    ctx: &ServiceContext,
    target: &FilePublish<'_>,
    snapshot: &Map<String, Value>,
    input: &mut WriteInput<'_>,
) -> Result<()> {
    let FilePublish { def, id, upload } = *target;

    let drafted = drafted_metadata(snapshot, upload);
    let Some(drafted_url) = drafted.get_str("url").map(str::to_string) else {
        return Ok(());
    };

    let live_url = stored_row(ctx, def, id, input.locale_ctx)?
        .and_then(|doc| doc.fields.get_str("url").map(str::to_string));

    if live_url.as_deref() == Some(drafted_url.as_str()) {
        return Ok(());
    }

    input.upload_conversions = Some(UploadConversions::new(
        deferred_conversions(&drafted, upload),
        ctx.image_max_attempts,
    ));

    // A translation may not carry shared columns in its own data (the locale
    // lock refuses them); its publish writes the drafted file's columns back
    // from the snapshot instead. The settle plan above is what it needs from
    // here, so the file's conversions are queued and the previous file's
    // cancelled exactly as on a default-locale publish.
    if query::is_non_default_single_locale(input.locale_ctx) {
        return Ok(());
    }

    input.data.extend(drafted);

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::upload::FormatQuality,
        db::{LocaleContext, LocaleMode},
        service::write::pending_draft::tests::{media, snapshot},
    };

    /// Only the server-derived columns are taken from the snapshot; a user
    /// field it also carries is left to the write's own data.
    #[test]
    fn drafted_metadata_takes_only_the_derived_columns() {
        let def = media();
        let upload = def.upload.as_ref().expect("upload");

        let drafted = drafted_metadata(&snapshot(), upload);

        assert_eq!(
            drafted.get("url"),
            Some(&json!("/uploads/media/abc_photo.png"))
        );
        assert_eq!(drafted.get("filename"), Some(&json!("abc_photo.png")));
        assert_eq!(drafted.get("thumbnail_width"), Some(&json!(300)));
        assert!(
            !drafted.contains_key("caption"),
            "a user field is not server-derived: {drafted:?}"
        );
    }

    /// The deferred variant is derived from the stored size file, so the job
    /// converts the drafted thumbnail — not the published row's.
    #[test]
    fn deferred_conversions_target_the_drafted_size_file() {
        let def = media();
        let upload = def.upload.as_ref().expect("upload");
        let drafted = drafted_metadata(&snapshot(), upload);

        let queued = deferred_conversions(&drafted, upload);

        assert_eq!(queued.len(), 1, "{queued:?}");
        assert_eq!(queued[0].source_path, "media/abc_photo_thumbnail.png");
        assert_eq!(queued[0].target_path, "media/abc_photo_thumbnail.webp");
        assert_eq!(queued[0].url_column, "thumbnail_webp_url");
    }

    /// A format converted during the upload owes the queue nothing — only a
    /// `queue = true` variant is deferred to the publish.
    #[test]
    fn a_synchronously_converted_format_queues_nothing() {
        let mut def = media();
        def.upload.as_mut().expect("upload").format_options.webp =
            Some(FormatQuality::new(80, false));

        let upload = def.upload.as_ref().expect("upload");

        let drafted = drafted_metadata(&snapshot(), upload);

        assert!(deferred_conversions(&drafted, upload).is_empty());
    }

    /// A request that carried a file of its own owns every server-derived
    /// column, including the ones its file does not produce. Regression: a
    /// `.txt` published over a pending image draft kept the drafted image's
    /// `width` and thumbnail urls, so the row described two files at once.
    #[test]
    fn a_request_with_its_own_file_excludes_every_derived_column() {
        let def = media();

        let own_file: DocumentFields = [("url".to_string(), json!("/uploads/media/new.txt"))]
            .into_iter()
            .collect();
        let excluded = excluded_columns(&def, &WriteInput::builder(own_file).build());

        for column in ["url", "width", "height", "thumbnail_url", "filename"] {
            assert!(excluded.contains(column), "{column} in {excluded:?}");
        }
        assert!(
            !excluded.contains("focal_x"),
            "the focal point is a user setting, not derived from the file: {excluded:?}"
        );

        assert!(
            excluded_columns(&def, &WriteInput::builder(DocumentFields::new()).build()).is_empty(),
            "a publish without a file of its own adopts the drafted file whole"
        );
    }

    /// The file adoption is narrower than the field adoption: a request that
    /// processed a file of its own wins, and a collection without uploads has
    /// no file columns at all. A translation adopts the drafted file too — the
    /// pending draft is one unit — its columns arriving through the snapshot
    /// write-back rather than the request data.
    #[test]
    fn only_a_publish_without_its_own_file_adopts_the_drafted_file() {
        let def = media();

        assert!(
            drafted_file_target(&def, &WriteInput::builder(DocumentFields::new()).build())
                .is_some()
        );

        let own_file: DocumentFields = [("url".to_string(), json!("/uploads/media/new.png"))]
            .into_iter()
            .collect();
        assert!(
            drafted_file_target(&def, &WriteInput::builder(own_file).build()).is_none(),
            "the file the request carried wins"
        );

        let de = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        };
        assert!(
            drafted_file_target(
                &def,
                &WriteInput::builder(DocumentFields::new())
                    .locale_ctx(Some(&de))
                    .build()
            )
            .is_some(),
            "a translation's publish makes the drafted file live and has to settle it"
        );

        assert!(
            drafted_file_target(
                &CollectionDefinition::new("posts"),
                &WriteInput::builder(DocumentFields::new()).build()
            )
            .is_none(),
            "a collection without uploads has no file columns"
        );
    }
}
