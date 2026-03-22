//! Upload handling: file validation, image resizing, and format conversion (WebP/AVIF).

mod collection_upload;
mod format;
mod image_fit;
mod image_size;
mod image_size_builder;
mod metadata;
mod process;
mod processed_upload;
mod processed_upload_builder;
mod queued_conversion;
mod queued_conversion_builder;
mod resize;
mod size_result;
mod size_result_builder;
mod uploaded_file;
mod uploaded_file_builder;
mod validate;

pub use collection_upload::CollectionUpload;
pub use format::{FormatOptions, FormatQuality, FormatResult};
pub use image_fit::ImageFit;
pub use image_size::ImageSize;
pub use image_size_builder::ImageSizeBuilder;
pub use metadata::{
    assemble_sizes_object, delete_upload_files, enqueue_conversions, inject_upload_metadata,
};
pub use process::{CleanupGuard, process_upload};
pub use processed_upload::ProcessedUpload;
pub use processed_upload_builder::ProcessedUploadBuilder;
pub use queued_conversion::QueuedConversion;
pub use queued_conversion_builder::QueuedConversionBuilder;
pub use resize::process_image_entry;
pub use size_result::SizeResult;
pub use size_result_builder::SizeResultBuilder;
pub use uploaded_file::UploadedFile;
pub use uploaded_file_builder::UploadedFileBuilder;
pub use validate::format_filesize;
