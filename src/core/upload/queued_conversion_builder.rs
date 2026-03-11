use super::queued_conversion::QueuedConversion;

/// Builder for [`QueuedConversion`].
///
/// `source_path` and `target_path` are taken in `new()`. `format`, `quality`,
/// `url_column`, and `url_value` are required via chained methods.
pub struct QueuedConversionBuilder {
    source_path: String,
    target_path: String,
    format: Option<String>,
    quality: Option<u8>,
    url_column: Option<String>,
    url_value: Option<String>,
}

impl QueuedConversionBuilder {
    pub fn new(source_path: impl Into<String>, target_path: impl Into<String>) -> Self {
        Self {
            source_path: source_path.into(),
            target_path: target_path.into(),
            format: None,
            quality: None,
            url_column: None,
            url_value: None,
        }
    }

    pub fn format(mut self, f: impl Into<String>) -> Self {
        self.format = Some(f.into());
        self
    }

    pub fn quality(mut self, q: u8) -> Self {
        self.quality = Some(q);
        self
    }

    pub fn url_column(mut self, c: impl Into<String>) -> Self {
        self.url_column = Some(c.into());
        self
    }

    pub fn url_value(mut self, v: impl Into<String>) -> Self {
        self.url_value = Some(v.into());
        self
    }

    pub fn build(self) -> QueuedConversion {
        QueuedConversion {
            source_path: self.source_path,
            target_path: self.target_path,
            format: self
                .format
                .expect("QueuedConversionBuilder: format is required"),
            quality: self
                .quality
                .expect("QueuedConversionBuilder: quality is required"),
            url_column: self
                .url_column
                .expect("QueuedConversionBuilder: url_column is required"),
            url_value: self
                .url_value
                .expect("QueuedConversionBuilder: url_value is required"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_queued_conversion_with_all_fields() {
        let conv = QueuedConversionBuilder::new("/src/img.jpg", "/out/img.webp")
            .format("webp")
            .quality(80)
            .url_column("webp_url")
            .url_value("/uploads/img.webp")
            .build();
        assert_eq!(conv.source_path, "/src/img.jpg");
        assert_eq!(conv.target_path, "/out/img.webp");
        assert_eq!(conv.format, "webp");
        assert_eq!(conv.quality, 80);
        assert_eq!(conv.url_column, "webp_url");
        assert_eq!(conv.url_value, "/uploads/img.webp");
    }

    #[test]
    #[should_panic(expected = "QueuedConversionBuilder: format is required")]
    fn panics_without_format() {
        QueuedConversionBuilder::new("s", "t")
            .quality(80)
            .url_column("c")
            .url_value("v")
            .build();
    }

    #[test]
    #[should_panic(expected = "QueuedConversionBuilder: quality is required")]
    fn panics_without_quality() {
        QueuedConversionBuilder::new("s", "t")
            .format("webp")
            .url_column("c")
            .url_value("v")
            .build();
    }

    #[test]
    #[should_panic(expected = "QueuedConversionBuilder: url_column is required")]
    fn panics_without_url_column() {
        QueuedConversionBuilder::new("s", "t")
            .format("webp")
            .quality(80)
            .url_value("v")
            .build();
    }

    #[test]
    #[should_panic(expected = "QueuedConversionBuilder: url_value is required")]
    fn panics_without_url_value() {
        QueuedConversionBuilder::new("s", "t")
            .format("webp")
            .quality(80)
            .url_column("c")
            .build();
    }
}
