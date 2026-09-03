//! Upload handling: file validation, image resizing, and format conversion (WebP/AVIF).

mod collection_upload;
mod exif;
mod format;
mod image_fit;
mod image_size;
mod metadata;
pub mod process;
mod processed_upload;
mod queue;
mod queued_conversion;
mod resize;
mod size_result;
pub mod storage;
mod uploaded_file;
mod validate;

pub use collection_upload::CollectionUpload;
pub use format::{FormatOptions, FormatQuality, FormatResult};
pub use image_fit::ImageFit;
pub use image_size::{ImageSize, ImageSizeBuilder};
pub use metadata::{
    assemble_sizes_object, delete_upload_files, enqueue_conversions, inject_upload_metadata,
};
pub use process::{CleanupGuard, process_upload};
pub use processed_upload::ProcessedUpload;
pub use queue::{
    FALLBACK_MAX_ATTEMPTS, IMAGE_CONVERT_QUEUE, ImageConvertJobData, SYSTEM_IMAGE_CONVERT_JOB,
    delete_image_jobs_for_document, queue_image_conversion,
};
pub use queued_conversion::QueuedConversion;
pub use resize::process_image_entry_with_storage;
pub use size_result::SizeResult;
pub use storage::{
    SharedStorage, StorageBackend, StorageNotFound, create_storage, create_storage_with_lease,
    key_from_served_url, served_url, sign_upload_path, signed_upload_url, verify_upload_sig,
};
pub use uploaded_file::UploadedFile;
pub use validate::format_filesize;
