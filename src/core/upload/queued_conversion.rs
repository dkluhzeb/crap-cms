use super::queued_conversion_builder::QueuedConversionBuilder;

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
    pub fn builder(source_path: impl Into<String>, target_path: impl Into<String>) -> QueuedConversionBuilder {
        QueuedConversionBuilder::new(source_path, target_path)
    }
}
