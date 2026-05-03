//! Typed page contexts for error pages (403 / 404 / 500).

use schemars::JsonSchema;
use serde::Serialize;

use super::BasePageContext;

/// Generic error page context. Templates branch on `page.type` (one of
/// `error_403`, `error_404`, `error_500`) to render the right shell.
#[derive(Serialize, JsonSchema)]
pub struct ErrorPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    /// User-facing error message body.
    pub message: String,
}
