use serde::{Deserialize, Serialize};

use super::{image_fit::ImageFit, image_size_builder::ImageSizeBuilder};

/// A named image resize target (e.g. "thumbnail" at 200x200).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSize {
    pub name: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub fit: ImageFit,
}

impl ImageSize {
    pub fn builder(name: impl Into<String>) -> ImageSizeBuilder {
        ImageSizeBuilder::new(name)
    }
}
