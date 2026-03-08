use serde::{Deserialize, Serialize};

/// How an image is resized to fit the target dimensions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ImageFit {
    #[default]
    Cover,
    Contain,
    Inside,
    Fill,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_fit_default_is_cover() {
        let fit = ImageFit::default();
        assert!(matches!(fit, ImageFit::Cover));
    }
}
