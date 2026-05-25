//! Named image resize target (e.g. "thumbnail" at 200x200).

use serde::{Deserialize, Serialize};

use crate::core::upload::ImageFit;
use crate::typegen::lua::LuaAnnotation;

/// A named image resize target (e.g. "thumbnail" at 200x200).
#[derive(Debug, Clone, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.ImageSize")]
pub struct ImageSize {
    /// Size name (e.g., "thumbnail", "card").
    pub name: String,
    /// Target width in pixels.
    pub width: u32,
    /// Target height in pixels.
    pub height: u32,
    /// Resize fit mode (default: "cover").
    #[serde(default)]
    #[lua(ty = "crap.ImageFit", optional)]
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

    #[must_use]
    pub fn width(mut self, w: u32) -> Self {
        self.width = Some(w);
        self
    }

    #[must_use]
    pub fn height(mut self, h: u32) -> Self {
        self.height = Some(h);
        self
    }

    #[must_use]
    pub fn fit(mut self, f: ImageFit) -> Self {
        self.fit = f;
        self
    }

    /// Build the final [`ImageSize`].
    ///
    /// # Panics
    ///
    /// Panics if `width` or `height` was not set on the builder.
    #[must_use]
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
        let _ = ImageSizeBuilder::new("thumb").height(100).build();
    }

    #[test]
    #[should_panic(expected = "ImageSizeBuilder: height is required")]
    fn panics_without_height() {
        let _ = ImageSizeBuilder::new("thumb").width(100).build();
    }
}
