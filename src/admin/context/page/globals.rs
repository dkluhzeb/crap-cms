//! Typed page contexts for global-singleton pages (edit, versions list,
//! restore-confirm).

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::BasePageContext;
use crate::admin::context::{FieldContext, GlobalContext, GlobalPermissions, PaginationContext};

/// `/admin/globals/{slug}` edit form context.
#[derive(Serialize, JsonSchema)]
pub struct GlobalEditPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub global: GlobalContext,
    pub perms: GlobalPermissions,
    pub fields: Vec<FieldContext>,
    pub sidebar_fields: Vec<FieldContext>,

    pub has_drafts: bool,
    pub has_versions: bool,
    pub versions: Vec<Value>,
    pub has_more_versions: bool,

    pub restore_url_prefix: String,
    pub versions_url: String,
    pub doc_status: String,
}

/// Slim re-render context for the `globals/edit` template after a validation
/// error. Mirrors [`super::collections::CollectionFormErrorPage`] for
/// globals.
#[derive(Serialize, JsonSchema)]
pub struct GlobalFormErrorPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub global: GlobalContext,
    pub perms: GlobalPermissions,
    pub fields: Vec<FieldContext>,
    pub sidebar_fields: Vec<FieldContext>,
}

/// `/admin/globals/{slug}/versions` versions-listing page context.
#[derive(Serialize, JsonSchema)]
pub struct GlobalVersionsListPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub global: GlobalContext,
    pub pagination: PaginationContext,

    pub versions: Vec<Value>,
    pub restore_url_prefix: String,
}

/// `/admin/globals/{slug}/versions/{ver}/restore` restore-confirmation page.
#[derive(Serialize, JsonSchema)]
pub struct GlobalRestoreConfirmPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub global: GlobalContext,

    pub version_number: Value,
    pub missing_relations: Vec<Value>,

    /// Storage keys the version names whose files are gone. A global is never
    /// an upload collection, so this is always empty — the field exists because
    /// both surfaces render the same confirmation partial.
    pub missing_files: Vec<String>,

    pub restore_url: String,
    pub back_url: String,
}
