//! Typed page contexts for global-singleton pages (edit, versions list,
//! restore-confirm).

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::{BasePageContext, RevisionConflictNotice};
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

    /// The global's revision the form was loaded at — submitted back as the
    /// save's precondition (`_revision`), so a save over someone else's newer
    /// change is refused instead of silently overwriting it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
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

    /// Always `true`: the form re-renders a submission that was not saved, so
    /// the unsaved-changes guard starts out armed.
    pub unsaved: bool,

    /// The revision the re-rendered form submits back (`_revision`): the one
    /// it was loaded at — or, after a revision conflict, the global's
    /// current one, so saving again overwrites on purpose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,

    /// Present when the save was refused because the global was saved by
    /// someone else after the form was loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_conflict: Option<RevisionConflictNotice>,
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
