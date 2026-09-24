//! The file-input half of an upload collection's form context, shared by the
//! create and edit forms.

use crate::{
    admin::context::page::collections::UploadFormContext,
    core::{CollectionDefinition, upload::format_filesize},
};

/// An [`UploadFormContext`] carrying what the file input needs — the accept
/// list (only when the collection declares mime types) and the size limit
/// (the collection's own `max_file_size`, else `global_max_file_size`). The
/// edit form adds the stored file's preview on top.
pub(in crate::admin::handlers::collections) fn upload_form_context(
    def: &CollectionDefinition,
    global_max_file_size: u64,
) -> UploadFormContext {
    let upload = def.upload.as_ref();

    let accept = upload
        .filter(|u| !u.mime_types.is_empty())
        .map(|u| u.mime_types.join(","));

    let max_file_size = def.max_upload_size(global_max_file_size);

    UploadFormContext {
        accept,
        max_file_size,
        max_file_size_display: format_filesize(max_file_size),
        ..UploadFormContext::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::upload::CollectionUpload;

    #[test]
    fn accept_joins_declared_mime_types() {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload {
            mime_types: vec!["image/png".into(), "image/jpeg".into()],
            ..Default::default()
        });

        assert_eq!(
            upload_form_context(&def, 1024).accept.as_deref(),
            Some("image/png,image/jpeg")
        );
    }

    #[test]
    fn accept_is_none_without_upload_or_with_no_mime_types() {
        let no_upload = CollectionDefinition::new("posts");
        assert!(upload_form_context(&no_upload, 1024).accept.is_none());

        let mut empty = CollectionDefinition::new("media");
        empty.upload = Some(CollectionUpload::default());
        assert!(upload_form_context(&empty, 1024).accept.is_none());
    }

    /// The file input learns the limit so it can refuse an oversized pick
    /// before the form is sent (the server would answer 413).
    #[test]
    fn the_size_limit_prefers_the_collection_over_the_global_default() {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload::new());

        let ctx = upload_form_context(&def, 2048);
        assert_eq!(ctx.max_file_size, 2048);
        assert_eq!(ctx.max_file_size_display, "2.0 KB");

        def.upload.as_mut().unwrap().max_file_size = Some(1024 * 1024);
        let ctx = upload_form_context(&def, 2048);
        assert_eq!(ctx.max_file_size, 1024 * 1024);
        assert_eq!(ctx.max_file_size_display, "1.0 MB");
    }
}
