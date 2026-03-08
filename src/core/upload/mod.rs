//! Upload handling: file validation, image resizing, and format conversion (WebP/AVIF).

mod image_fit;
mod image_size;
mod image_size_builder;
mod format;
mod collection_upload;
mod uploaded_file;
mod uploaded_file_builder;
mod queued_conversion;
mod queued_conversion_builder;
mod size_result;
mod size_result_builder;
mod processed_upload;
mod processed_upload_builder;
mod validate;
mod process;
mod resize;
mod metadata;

pub use image_fit::ImageFit;
pub use image_size::ImageSize;
pub use image_size_builder::ImageSizeBuilder;
pub use format::{FormatOptions, FormatQuality, FormatResult};
pub use collection_upload::CollectionUpload;
pub use uploaded_file::UploadedFile;
pub use uploaded_file_builder::UploadedFileBuilder;
pub use queued_conversion::QueuedConversion;
pub use queued_conversion_builder::QueuedConversionBuilder;
pub use size_result::SizeResult;
pub use size_result_builder::SizeResultBuilder;
pub use processed_upload::ProcessedUpload;
pub use processed_upload_builder::ProcessedUploadBuilder;
pub use validate::format_filesize;
pub use process::process_upload;
pub use resize::process_image_entry;
pub use metadata::{assemble_sizes_object, inject_upload_metadata, delete_upload_files, enqueue_conversions};
