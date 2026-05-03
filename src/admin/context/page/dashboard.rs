//! Typed page context for the dashboard.

use schemars::JsonSchema;
use serde::Serialize;

use super::BasePageContext;

/// One collection summary card on the dashboard. The shape mirrors the
/// keys the dashboard template reads.
#[derive(Serialize, JsonSchema)]
pub struct CollectionCard {
    pub slug: String,
    pub display_name: String,
    pub singular_name: String,
    pub count: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,

    pub is_auth: bool,
    pub is_upload: bool,
    pub has_versions: bool,
}

/// One global summary card on the dashboard.
#[derive(Serialize, JsonSchema)]
pub struct GlobalCard {
    pub slug: String,
    pub display_name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,

    pub has_versions: bool,
}

/// Dashboard page context.
#[derive(Serialize, JsonSchema)]
pub struct DashboardPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub collection_cards: Vec<CollectionCard>,
    pub global_cards: Vec<GlobalCard>,
}
