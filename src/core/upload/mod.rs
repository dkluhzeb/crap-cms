//! Upload handling: file validation, image resizing, and format conversion (WebP/AVIF).

mod collection_upload;
mod format;
mod image_fit;
mod image_size;
mod metadata;
pub mod process;
mod processed_upload;
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
pub use processed_upload::{ProcessedUpload, ProcessedUploadBuilder};
pub use queued_conversion::{QueuedConversion, QueuedConversionBuilder};
pub use resize::process_image_entry_with_storage;
pub use size_result::{SizeResult, SizeResultBuilder};
pub use storage::{SharedStorage, StorageBackend, create_storage};
pub use uploaded_file::{UploadedFile, UploadedFileBuilder};
pub use validate::format_filesize;
