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
