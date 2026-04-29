//! Typed page contexts for collection-related pages (list, items, edit,
//! create, delete-confirm, versions, restore-confirm).

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::BasePageContext;
use crate::admin::context::{
    CollectionContext, DocumentRef, FieldContext, LocaleTemplateData, PaginationContext,
};

/// One row on the `/admin/collections` listing page.
#[derive(Serialize, JsonSchema)]
pub struct CollectionEntry {
    pub slug: String,
    pub display_name: String,
    pub field_count: usize,
}

/// `/admin/collections` page context.
#[derive(Serialize, JsonSchema)]
pub struct CollectionListPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub collections: Vec<CollectionEntry>,
}

/// `/admin/collections/{slug}` items-listing page context.
///
/// Several fields (`docs`, `table_columns`, `column_options`, `filter_fields`,
/// `active_filters`) are still `Vec<Value>` because their downstream builders
/// (`compute_cells`, `build_column_options`, `build_filter_fields`,
/// `build_filter_pills`) haven't been migrated to typed structs yet — that's
/// independent surgery. The page-level shape is fixed; tightening the
/// inner types is a future cleanup.
#[derive(Serialize, JsonSchema)]
pub struct CollectionItemsListPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub collection: CollectionContext,
    pub docs: Vec<Value>,
    pub pagination: PaginationContext,

    pub has_drafts: bool,
    pub has_soft_delete: bool,
    pub is_trash: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,

    pub table_columns: Vec<Value>,
    pub column_options: Vec<Value>,
    pub filter_fields: Vec<Value>,
    pub active_filters: Vec<Value>,
    pub active_filter_count: usize,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_sort_url: Option<String>,

    pub title_sorted_asc: bool,
    pub title_sorted_desc: bool,
}

/// Upload-collection preview block flattened onto the edit form when
/// `def.upload` is set.
#[derive(Serialize, Default, JsonSchema)]
pub struct UploadFormContext {
    /// Comma-joined accept list for the file input — emitted only when the
    /// collection declares allowed mime types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accept: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub focal_x: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub focal_y: Option<f64>,

    /// Image preview URL when the file is an image.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,

    /// Filename + dimensions/filesize info pill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<UploadInfo>,
}

#[derive(Serialize, JsonSchema)]
pub struct UploadInfo {
    pub filename: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesize_display: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<String>,
}

/// `/admin/collections/{slug}/{id}` edit form context.
#[derive(Serialize, JsonSchema)]
pub struct CollectionEditPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub collection: CollectionContext,
    pub document: DocumentRef,
    pub fields: Vec<FieldContext>,
    pub sidebar_fields: Vec<FieldContext>,

    pub editing: bool,
    pub has_drafts: bool,
    pub has_versions: bool,
    pub versions: Vec<Value>,
    pub has_more_versions: bool,

    pub restore_url_prefix: String,
    pub versions_url: String,
    pub document_title: String,
    pub ref_count: i64,

    /// Locale picker data (flattened: `has_locales`, `current_locale`,
    /// `locales`). Absent when locale support is disabled.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub locale_data: Option<LocaleTemplateData>,

    /// Upload preview block — present only on upload collections.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload: Option<UploadFormContext>,
}

/// `/admin/collections/{slug}/create` create form context.
#[derive(Serialize, JsonSchema)]
pub struct CollectionCreatePage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub collection: CollectionContext,
    pub fields: Vec<FieldContext>,
    pub sidebar_fields: Vec<FieldContext>,

    pub editing: bool,
    pub has_drafts: bool,

    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub locale_data: Option<LocaleTemplateData>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload: Option<UploadFormContext>,
}

/// Slim re-render context for the `collections/edit` template after a
/// validation / upload error. Carries only what the template needs in the
/// error path (no version sidebar, no breadcrumbs, no editor-locale data) —
/// the user is bounced back to the form they just submitted.
#[derive(Serialize, JsonSchema)]
pub struct CollectionFormErrorPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub collection: CollectionContext,

    /// Document stub (with `id` only) on edit error; absent on create error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<DocumentRef>,

    pub fields: Vec<FieldContext>,
    pub sidebar_fields: Vec<FieldContext>,

    pub editing: bool,
    pub has_drafts: bool,

    /// Hidden upload fields preserved from the submitted form (edit-mode
    /// upload errors only, so the user keeps their pending file metadata).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_hidden_fields: Option<Vec<Value>>,
}

/// `/admin/collections/{slug}/{id}/delete` delete-confirmation page.
#[derive(Serialize, JsonSchema)]
pub struct CollectionDeleteConfirmPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub collection: CollectionContext,
    pub document_id: String,
    /// Document title for display. `None` (serialized as `null`) when the
    /// collection has no title field or the read fell through.
    pub title_value: Option<String>,
    pub ref_count: i64,
}

/// `/admin/collections/{slug}/{id}/versions/{ver}/restore` restore-
/// confirmation page.
#[derive(Serialize, JsonSchema)]
pub struct CollectionRestoreConfirmPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub collection: CollectionContext,
    pub document: DocumentRef,

    /// Version number being restored (from the version row's `version`
    /// column).
    pub version_number: Value,

    /// IDs of relationship references whose targets no longer exist.
    pub missing_relations: Vec<Value>,

    pub restore_url: String,
    pub back_url: String,
}

/// `/admin/collections/{slug}/{id}/versions` versions-listing page context.
#[derive(Serialize, JsonSchema)]
pub struct CollectionVersionsListPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub collection: CollectionContext,
    pub document: DocumentRef,
    pub pagination: PaginationContext,

    pub doc_title: String,
    pub versions: Vec<Value>,
    pub restore_url_prefix: String,
}
