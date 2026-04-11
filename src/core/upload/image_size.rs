//! Named image resize target (e.g. "thumbnail" at 200x200).

use serde::{Deserialize, Serialize};

use crate::core::upload::ImageFit;

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
    /// Start building a new `ImageSize`.
    pub fn builder(name: impl Into<String>) -> ImageSizeBuilder {
        ImageSizeBuilder::new(name)
    }
}

/// Builder for [`ImageSize`].
///
/// `name` is taken in `new()`. `width` and `height` are required via chained methods.
/// `fit` defaults to [`ImageFit::Cover`].
pub struct ImageSizeBuilder {
    name: String,
    width: Option<u32>,
    height: Option<u32>,
    fit: ImageFit,
}

impl ImageSizeBuilder {
    /// Create a new builder with the required `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            width: None,
            height: None,
            fit: ImageFit::Cover,
        }
    }

    pub fn width(mut self, w: u32) -> Self {
        self.width = Some(w);
        self
    }

    pub fn height(mut self, h: u32) -> Self {
        self.height = Some(h);
        self
    }

    pub fn fit(mut self, f: ImageFit) -> Self {
        self.fit = f;
        self
    }

    /// Build the final [`ImageSize`].
    pub fn build(self) -> ImageSize {
        ImageSize {
            name: self.name,
            width: self.width.expect("ImageSizeBuilder: width is required"),
            height: self.height.expect("ImageSizeBuilder: height is required"),
            fit: self.fit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_image_size_with_required_fields() {
        let size = ImageSizeBuilder::new("thumbnail")
            .width(200)
            .height(200)
            .build();
        assert_eq!(size.name, "thumbnail");
        assert_eq!(size.width, 200);
        assert_eq!(size.height, 200);
        assert!(matches!(size.fit, ImageFit::Cover));
    }

    #[test]
    fn builds_image_size_with_contain_fit() {
        let size = ImageSizeBuilder::new("banner")
            .width(1200)
            .height(400)
            .fit(ImageFit::Contain)
            .build();
        assert!(matches!(size.fit, ImageFit::Contain));
    }

    #[test]
    #[should_panic(expected = "ImageSizeBuilder: width is required")]
    fn panics_without_width() {
        ImageSizeBuilder::new("thumb").height(100).build();
    }

    #[test]
    #[should_panic(expected = "ImageSizeBuilder: height is required")]
    fn panics_without_height() {
        ImageSizeBuilder::new("thumb").width(100).build();
    }
}
