use serde::{Deserialize, Serialize};

use crate::typegen::lua::LuaAlias;

/// Resize fit mode for image processing.
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAlias)]
#[serde(rename_all = "lowercase")]
#[lua(alias = "crap.ImageFit", rename_all = "lowercase")]
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
