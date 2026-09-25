//! Upload handling: file validation, image resizing, and format conversion (WebP/AVIF).

mod cleanup_guard;
mod collection_upload;
mod decode;
mod exif;
mod format;
mod image_fit;
mod image_size;
mod inspect;
mod metadata;
pub mod process;
mod processed_upload;
mod queue;
mod queued_conversion;
mod read_shape;
mod resize;
mod size_result;
pub mod storage;
mod stored_name;
mod svg;
mod uploaded_file;
mod validate;

pub use cleanup_guard::CleanupGuard;
pub use collection_upload::CollectionUpload;
pub use decode::{ImageProcessingBusy, set_image_concurrency};
pub use format::{FormatOptions, FormatQuality, FormatResult};
pub use image_fit::ImageFit;
pub use image_size::{ImageSize, ImageSizeBuilder};
pub use inspect::{FileColumns, InspectedUpload, inspect_upload};
pub use metadata::{
    assemble_sizes_object, delete_storage_keys, delete_upload_files, enqueue_conversions,
    inject_upload_metadata, shape_read_document, snapshot_file_keys, upload_file_entries,
    upload_file_keys,
};
pub use process::process_upload;
pub use processed_upload::ProcessedUpload;
pub use queue::{
    FALLBACK_MAX_ATTEMPTS, IMAGE_CONVERT_QUEUE, ImageConvertJobData, SYSTEM_IMAGE_CONVERT_JOB,
    delete_image_jobs_for_document, queue_image_conversion,
};
pub use queued_conversion::QueuedConversion;
pub use read_shape::{
    SIZES_FIELD, read_shape_fields, readable_fields, writable_fields, write_shape_fields,
};
pub use resize::process_image_entry_with_storage;
pub use size_result::SizeResult;
pub use storage::{
    ByteRange, ObjectMeta, RangedObject, SharedStorage, StorageBackend, StorageNotFound,
    StorageStat, create_storage, create_storage_with_lease, is_staging_file_name,
    key_from_served_url, served_url, sign_upload_path, signed_upload_url, slice_locally,
    verify_upload_sig,
};
pub use stored_name::{STORED_ID_LEN, original_filename};
pub use uploaded_file::UploadedFile;
pub use validate::format_filesize;
