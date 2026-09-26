//! Typed page contexts for collection-related pages (list, items, edit,
//! create, delete-confirm, versions, restore-confirm).

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::{BasePageContext, RevisionConflictNotice};
use crate::admin::context::{
    CollectionContext, CollectionPermissions, DocumentRef, FieldContext, PaginationContext,
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
    pub perms: CollectionPermissions,
    pub docs: Vec<Value>,
    pub pagination: PaginationContext,

    pub has_drafts: bool,
    pub has_soft_delete: bool,
    pub is_trash: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,

    /// Hidden `{name, value}` inputs the search form carries so a search keeps
    /// the sort, page size, filters, and trash view.
    pub search_params: Vec<Value>,

    /// Page 1 of the same view without the search term.
    pub clear_search_url: String,

    /// Whether a search or filter narrows the list (selects the "no results"
    /// empty state instead of "no items yet").
    pub is_filtered: bool,

    /// Trash view only: how many documents are in the trash regardless of the
    /// search and filters — what "Empty trash" deletes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trash_total: Option<i64>,

    pub table_columns: Vec<Value>,
    pub column_options: Vec<Value>,
    pub filter_fields: Vec<Value>,
    pub active_filters: Vec<Value>,
    pub active_filter_count: usize,

    /// Header label of the title column — the `use_as_title` field's label;
    /// absent when the column shows document ids.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_label: Option<String>,

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

    /// The largest file the collection accepts, in bytes — the file input
    /// refuses a larger pick before the form is sent.
    pub max_file_size: u64,

    /// The same limit, formatted for display (`50.0 MB`).
    pub max_file_size_display: String,

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
    pub perms: CollectionPermissions,
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

    /// Upload preview block — present only on upload collections.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload: Option<UploadFormContext>,

    /// The document revision the form was loaded at — submitted back as the
    /// save's precondition (`_revision`), so a save over someone else's newer
    /// change is refused instead of silently overwriting it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
}

/// `/admin/collections/{slug}/create` create form context.
#[derive(Serialize, JsonSchema)]
pub struct CollectionCreatePage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub collection: CollectionContext,
    pub perms: CollectionPermissions,
    pub fields: Vec<FieldContext>,
    pub sidebar_fields: Vec<FieldContext>,

    pub editing: bool,
    pub has_drafts: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload: Option<UploadFormContext>,
}

/// Slim re-render context for the `collections/edit` template after a
/// validation / upload error. Carries only what the template needs in the
/// error path (no version sidebar, no breadcrumbs) — the user is bounced back
/// to the form they just submitted.
///
/// The `base` must be built with the *submitted* locale, exactly as the
/// success-path forms build theirs: the re-rendered form has to stay in the
/// locale it was submitted in — the template's hidden `_locale` input reads
/// `editor_locale` — or the corrected save writes a translation into the
/// default locale's columns and overwrites the shared fields.
#[derive(Serialize, JsonSchema)]
pub struct CollectionFormErrorPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    pub collection: CollectionContext,
    pub perms: CollectionPermissions,

    /// Document stub (with `id` only) on edit error; absent on create error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<DocumentRef>,

    pub fields: Vec<FieldContext>,
    pub sidebar_fields: Vec<FieldContext>,

    pub editing: bool,
    pub has_drafts: bool,

    /// Always `true`: the form re-renders a submission that was not saved, so
    /// the unsaved-changes guard starts out armed.
    pub unsaved: bool,

    /// Hidden upload inputs preserved from the submitted form — the focal point
    /// the edit page renders inside its file-preview block, which this slim
    /// context does not carry. Without them a failed save resets the focal
    /// point the user just moved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_hidden_fields: Option<Vec<Value>>,

    /// The revision the re-rendered form submits back (`_revision`): the one
    /// it was loaded at — or, after a revision conflict, the document's
    /// current one, so saving again overwrites on purpose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,

    /// Present when the save was refused because the document was saved by
    /// someone else after the form was loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_conflict: Option<RevisionConflictNotice>,
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

    /// Storage keys the version names whose files are gone. Always empty for a
    /// collection without uploads.
    pub missing_files: Vec<String>,

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

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::to_value;

    use super::*;
    use crate::{
        admin::{
            context::{PageMeta, PageType},
            test_state::test_admin_state,
        },
        config::LocaleConfig,
        core::CollectionDefinition,
    };

    /// The edit/create forms carry exactly ONE locale-picker shape: the
    /// editor-locale trio flattened in from the base context, built from the
    /// same locale the page reads in. The parallel `has_locales` /
    /// `current_locale` / `locales` keys are gone, so an override template
    /// cannot bind to a second copy that could drift out of step with it.
    #[test]
    fn create_page_carries_only_the_editor_locale_keys() {
        let mut state = test_admin_state();
        state.config.locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: false,
        };

        let def = CollectionDefinition::builder("posts").build();
        let base = BasePageContext::for_handler(
            &state,
            None,
            None,
            PageMeta::new(PageType::CollectionCreate, "create_name"),
        )
        .with_editor_locale(Some("de"), &state);

        let ctx = CollectionCreatePage {
            base,
            collection: CollectionContext::from_def(&def),
            perms: CollectionPermissions::default(),
            fields: Vec::new(),
            sidebar_fields: Vec::new(),
            editing: false,
            has_drafts: false,
            upload: None,
        };

        let json = to_value(&ctx).expect("page context serializes");

        assert_eq!(json["has_editor_locales"], Value::Bool(true));
        assert_eq!(json["editor_locale"], Value::String("de".to_string()));
        assert_eq!(
            json["editor_locales"][1]["value"],
            Value::String("de".to_string())
        );
        assert_eq!(json["editor_locales"][1]["selected"], Value::Bool(true));

        for gone in ["has_locales", "current_locale", "locales"] {
            assert!(
                json.get(gone).is_none(),
                "`{gone}` is the removed twin of the editor-locale keys"
            );
        }
    }
}
