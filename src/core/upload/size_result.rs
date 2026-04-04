//! Output metadata for one generated image size.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::core::upload::FormatResult;

/// Output metadata for one generated image size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizeResult {
    pub url: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub formats: HashMap<String, FormatResult>,
}

impl SizeResult {
    /// Start building a new `SizeResult`.
    pub fn builder(url: impl Into<String>) -> SizeResultBuilder {
        SizeResultBuilder::new(url)
    }
}

/// Builder for [`SizeResult`].
///
/// `url` is taken in `new()`. `width` and `height` are required via chained methods.
/// `formats` defaults to an empty map.
pub struct SizeResultBuilder {
    url: String,
    width: Option<u32>,
    height: Option<u32>,
    formats: HashMap<String, FormatResult>,
}

impl SizeResultBuilder {
    /// Create a new builder with the required `url`.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            width: None,
            height: None,
            formats: HashMap::new(),
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

    pub fn formats(mut self, f: HashMap<String, FormatResult>) -> Self {
        self.formats = f;
        self
    }

    /// Build the final [`SizeResult`].
    pub fn build(self) -> SizeResult {
        SizeResult {
            url: self.url,
            width: self.width.expect("SizeResultBuilder: width is required"),
            height: self.height.expect("SizeResultBuilder: height is required"),
            formats: self.formats,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_size_result_with_required_fields() {
        let result = SizeResultBuilder::new("/uploads/thumb.jpg")
            .width(200)
            .height(200)
            .build();
        assert_eq!(result.url, "/uploads/thumb.jpg");
        assert_eq!(result.width, 200);
        assert_eq!(result.height, 200);
        assert!(result.formats.is_empty());
    }

    #[test]
    fn builds_size_result_with_formats() {
        let mut formats = HashMap::new();
        formats.insert("webp".to_string(), FormatResult::new("/t.webp"));
        let result = SizeResultBuilder::new("/t.jpg")
            .width(100)
            .height(100)
            .formats(formats)
            .build();
        assert_eq!(result.formats.len(), 1);
        assert_eq!(result.formats["webp"].url, "/t.webp");
    }

    #[test]
    #[should_panic(expected = "SizeResultBuilder: width is required")]
    fn panics_without_size_result_width() {
        SizeResultBuilder::new("/t.jpg").height(100).build();
    }

    #[test]
    #[should_panic(expected = "SizeResultBuilder: height is required")]
    fn panics_without_size_result_height() {
        SizeResultBuilder::new("/t.jpg").width(100).build();
    }
}
