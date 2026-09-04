//! Form error rendering — unified handling for upload errors and validation errors.

use std::collections::HashMap;

use axum::{Extension, response::Response};
use serde_json::{Map, Value, json};

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, CollectionContext, CollectionPermissions, DocumentRef, PageMeta,
            PageType, page::collections::CollectionFormErrorPage,
        },
        handlers::{
            forms::FormData,
            shared::{
                EnrichOptions, apply_display_conditions, build_field_contexts,
                enrich_field_contexts, forbidden, get_user_doc, page_with_toast, paths,
                redirect_response, split_sidebar_fields, toast_only_error,
                translate_validation_errors,
            },
        },
    },
    core::{AuthUser, CollectionDefinition, FieldDefinition, ValidationError},
    hooks::ConditionContext,
    service::ServiceError,
};
use tracing::error;

/// Decide the toast message for a write error that fell through the form's typed
/// `AccessDenied` / `Validation` arms. A Lua hook abort reaches the admin path as
/// `Internal` (the form handlers don't run `classify`), so re-classify to surface
/// the hook's own message. Genuine internals (DB, pool, bugs) are logged and get a
/// generic message — so we never leak internal error text *and* never silently
/// redirect. `operation` is only used for the log line.
pub(in crate::admin::handlers::collections) fn write_error_toast(
    operation: &str,
    err: ServiceError,
    db_kind: &str,
) -> String {
    let classified = err.reclassify(db_kind);

    if let ServiceError::HookError(msg) = classified {
        return msg;
    }

    error!("{operation} error: {classified}");
    "Something went wrong while saving. Please try again.".to_string()
}

/// Collect hidden upload field values from form data for re-rendering after validation errors.
pub(in crate::admin::handlers::collections) fn collect_upload_hidden_fields(
    fields: &[FieldDefinition],
    form_data: &HashMap<String, String>,
) -> Value {
    let hidden_fields: Vec<Value> = fields
        .iter()
        .filter(|f| f.admin.hidden)
        .filter_map(|f| {
            form_data
                .get(&f.name)
                .map(|v| json!({"name": &f.name, "value": v}))
        })
        .collect();

    json!(hidden_fields)
}

/// Parameters for re-rendering a form with errors.
pub(in crate::admin::handlers::collections) struct FormErrorParams<'a> {
    pub state: &'a AdminState,
    pub def: &'a CollectionDefinition,
    pub form: &'a FormData,
    pub error_map: &'a HashMap<String, String>,
    pub doc_id: Option<&'a str>,
    pub auth_user: Option<&'a Extension<AuthUser>>,
    pub toast_msg: &'a str,
}

/// Build and render the form with an error toast. Handles both create (`doc_id = None`)
/// and edit (`doc_id = Some(id)`) modes, including upload hidden field preservation.
pub(in crate::admin::handlers::collections) async fn render_form_with_error(
    p: &FormErrorParams<'_>,
) -> Response {
    let mut fields = build_field_contexts(&p.def.fields, p.form.raw(), p.error_map, true, false);

    let enrich_opts = EnrichOptions::builder(p.error_map)
        .filter_hidden(true)
        .doc_id(p.doc_id)
        .user(get_user_doc(p.auth_user));

    enrich_field_contexts(
        &mut fields,
        &p.def.fields,
        p.form.join(),
        p.state,
        &enrich_opts.build(),
    );

    let form_json = if p.doc_id.is_some() {
        json!(
            p.form
                .raw()
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect::<Map<String, Value>>()
        )
    } else {
        json!({})
    };

    let cond_ctx = ConditionContext {
        collection: &p.def.slug,
        operation: if p.doc_id.is_some() {
            "update"
        } else {
            "create"
        },
        user: get_user_doc(p.auth_user),
        ui_locale: p.auth_user.map(|Extension(au)| au.ui_locale.as_str()),
        locale: None,
        options: None,
    };

    apply_display_conditions(
        &mut fields,
        &p.def.fields,
        &form_json,
        &p.state.infra.hook_runner,
        true,
        &cond_ctx,
    );

    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    let editing = p.doc_id.is_some();
    let (page_type, page_key) = if editing {
        (PageType::CollectionEdit, "edit_name")
    } else {
        (PageType::CollectionCreate, "create_name")
    };

    let base = BasePageContext::for_handler(
        p.state,
        None,
        p.auth_user,
        PageMeta::new(page_type, page_key).with_title_name(p.def.singular_name()),
    );

    let upload_hidden_fields = (editing && p.def.is_upload_collection()).then(|| {
        let value = collect_upload_hidden_fields(&p.def.fields, p.form.raw());
        match value {
            Value::Array(arr) => arr,
            _ => Vec::new(),
        }
    });

    let perms = CollectionPermissions::for_user(p.state, p.def, p.auth_user);

    let ctx = CollectionFormErrorPage {
        base,
        collection: CollectionContext::from_def(p.def),
        perms,
        document: p.doc_id.map(DocumentRef::stub),
        fields: main_fields,
        sidebar_fields,
        editing,
        has_drafts: p.def.has_drafts(),
        upload_hidden_fields,
    };

    page_with_toast(p.state, p.auth_user, "collections/edit", &ctx, p.toast_msg).await
}

/// Render the upload error page (create mode).
pub(in crate::admin::handlers::collections) async fn render_upload_error(
    state: &AdminState,
    def: &CollectionDefinition,
    form_data: &HashMap<String, String>,
    auth_user: Option<&Extension<AuthUser>>,
    err_msg: &str,
) -> Response {
    let form = FormData::raw_only(form_data.clone());

    render_form_with_error(&FormErrorParams {
        state,
        def,
        form: &form,
        error_map: &HashMap::new(),
        doc_id: None,
        auth_user,
        toast_msg: err_msg,
    })
    .await
}

/// Render the upload error page (edit mode).
pub(in crate::admin::handlers::collections) async fn render_edit_upload_error(
    state: &AdminState,
    def: &CollectionDefinition,
    form_data: &HashMap<String, String>,
    id: &str,
    auth_user: Option<&Extension<AuthUser>>,
    err_msg: &str,
) -> Response {
    let form = FormData::raw_only(form_data.clone());

    render_form_with_error(&FormErrorParams {
        state,
        def,
        form: &form,
        error_map: &HashMap::new(),
        doc_id: Some(id),
        auth_user,
        toast_msg: err_msg,
    })
    .await
}

/// Re-render the form with validation errors (works for both create and edit).
pub(in crate::admin::handlers::collections) async fn render_form_validation_errors(
    state: &AdminState,
    def: &CollectionDefinition,
    doc_id: Option<&str>,
    form: &FormData,
    ve: &ValidationError,
    auth_user: Option<&Extension<AuthUser>>,
) -> Response {
    let locale = auth_user.map_or("en", |Extension(au)| au.ui_locale.as_str());

    let error_map = translate_validation_errors(ve, &state.translations, locale);
    let toast_msg = state.translations.get(locale, "validation.error_summary");

    render_form_with_error(&FormErrorParams {
        state,
        def,
        form,
        error_map: &error_map,
        doc_id,
        auth_user,
        toast_msg,
    })
    .await
}

/// Parameters for mapping a collection write `ServiceError` to a form response.
pub(in crate::admin::handlers::collections) struct WriteErrorParams<'a> {
    pub state: &'a AdminState,
    pub def: &'a CollectionDefinition,
    pub form: &'a FormData,
    pub err: ServiceError,
    pub doc_id: Option<&'a str>,
    pub auth_user: Option<&'a Extension<AuthUser>>,
}

/// Which form response a collection write error maps to. Split out from the
/// rendering so the create-vs-edit branch decisions are unit-testable without
/// constructing an `AdminState`.
#[derive(Debug, PartialEq, Eq)]
enum WriteErrorResponse {
    /// 403 — the user lacks permission for this write.
    Forbidden,
    /// Re-render the form with inline field errors.
    Validation,
    /// The edited document vanished (deleted in another tab) — navigate back.
    RedirectToItem,
    /// Generic error toast over the re-rendered form.
    Toast,
}

/// Decide how a collection write error maps to a response. `editing` is true for
/// update (a target document exists), false for create.
///
/// A `NotFound` is a navigation case only when editing; on create there is no
/// target document, so an unexpected `NotFound` is a real error and toasts like
/// any other. This is the one place the create/edit branch is decided, so the
/// two write handlers can't drift.
fn classify_write_error(editing: bool, err: &ServiceError) -> WriteErrorResponse {
    match err {
        ServiceError::AccessDenied(_) => WriteErrorResponse::Forbidden,
        ServiceError::Validation(_) => WriteErrorResponse::Validation,
        ServiceError::NotFound(_) if editing => WriteErrorResponse::RedirectToItem,
        _ => WriteErrorResponse::Toast,
    }
}

/// The access-denied message for the write surface — create vs edit wording.
fn access_denied_msg(editing: bool) -> &'static str {
    if editing {
        "You don't have permission to update this item"
    } else {
        "You don't have permission to create items in this collection"
    }
}

/// The operation label used in the fallback error toast / log line.
fn op_label(editing: bool) -> &'static str {
    if editing { "Update" } else { "Create" }
}

/// Map a collection create/update `ServiceError` to the correct form response,
/// so both write handlers classify errors identically instead of each spelling
/// the `AccessDenied` / `Validation` / `NotFound` / fallback arms by hand.
///
/// `doc_id` distinguishes edit (`Some`) from create (`None`) — see
/// [`classify_write_error`] for the branch decisions.
pub(in crate::admin::handlers::collections) async fn handle_collection_write_error(
    p: WriteErrorParams<'_>,
) -> Response {
    let editing = p.doc_id.is_some();

    match classify_write_error(editing, &p.err) {
        WriteErrorResponse::Forbidden => forbidden(p.state, access_denied_msg(editing)),
        WriteErrorResponse::RedirectToItem => {
            let id = p.doc_id.unwrap_or_default();

            redirect_response(&paths::collection_item(&p.def.slug, id))
        }
        WriteErrorResponse::Validation => {
            if let ServiceError::Validation(ref ve) = p.err {
                return render_form_validation_errors(
                    p.state,
                    p.def,
                    p.doc_id,
                    p.form,
                    ve,
                    p.auth_user,
                )
                .await;
            }

            toast_only_error(&write_error_toast(
                op_label(editing),
                p.err,
                p.state.infra.pool.kind(),
            ))
        }
        WriteErrorResponse::Toast => toast_only_error(&write_error_toast(
            op_label(editing),
            p.err,
            p.state.infra.pool.kind(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use crate::core::field::{FieldAdmin, FieldType};

    use super::*;

    /// A Lua hook abort arrives as `Internal("runtime error: …")`; the toast must
    /// surface the hook's own message (so e.g. "price may only increase" reaches
    /// the user), not a generic one.
    #[test]
    fn write_error_toast_surfaces_hook_message() {
        let err = ServiceError::Internal(anyhow!(
            "runtime error: hooks/guard.lua:3: price may only increase"
        ));
        let msg = write_error_toast("Create", err, "sqlite");
        assert!(
            msg.contains("price may only increase"),
            "hook message must reach the user, got: {msg}"
        );
    }

    /// A direct `HookError` (e.g. the gRPC-classified form) passes through.
    #[test]
    fn write_error_toast_passes_through_hook_error() {
        let msg = write_error_toast("Update", ServiceError::HookError("nope".into()), "sqlite");
        assert_eq!(msg, "nope");
    }

    /// A genuine internal error must NOT leak its text — generic message only.
    #[test]
    fn write_error_toast_hides_internal_detail() {
        let err = ServiceError::Internal(anyhow!("disk I/O failure at /var/lib/db.sqlite"));
        let msg = write_error_toast("Create", err, "sqlite");
        assert!(msg.contains("Something went wrong"), "got: {msg}");
        assert!(
            !msg.contains("disk I/O") && !msg.contains("db.sqlite"),
            "must not leak internal error text, got: {msg}"
        );
    }

    /// Regression: a `NotFound` maps to a back-navigation redirect ONLY when
    /// editing. On create there is no target document, so an unexpected
    /// `NotFound` must fall through to a toast — not silently redirect. Before
    /// the shared mapper, only the update handler had a `NotFound` arm and the
    /// create/edit branch was spelled by hand in each handler.
    #[test]
    fn not_found_redirects_only_when_editing() {
        assert_eq!(
            classify_write_error(true, &ServiceError::NotFound("gone".into())),
            WriteErrorResponse::RedirectToItem,
            "editing: vanished target is a navigation case"
        );
        assert_eq!(
            classify_write_error(false, &ServiceError::NotFound("gone".into())),
            WriteErrorResponse::Toast,
            "create: no target, NotFound is a real error → toast, never redirect"
        );
    }

    /// The shared arms are identical across create and edit: access-denied →
    /// forbidden, validation → inline errors, everything else → toast.
    #[test]
    fn shared_arms_agree_across_create_and_edit() {
        for editing in [false, true] {
            assert_eq!(
                classify_write_error(editing, &ServiceError::AccessDenied("no".into())),
                WriteErrorResponse::Forbidden
            );
            assert_eq!(
                classify_write_error(
                    editing,
                    &ServiceError::Validation(ValidationError::new(vec![]))
                ),
                WriteErrorResponse::Validation
            );
            assert_eq!(
                classify_write_error(editing, &ServiceError::HookError("boom".into())),
                WriteErrorResponse::Toast
            );
        }
    }

    /// The operation label and access-denied wording differ by surface so the
    /// toast log line and the 403 message stay create/edit-specific.
    #[test]
    fn labels_differ_by_surface() {
        assert_eq!(op_label(false), "Create");
        assert_eq!(op_label(true), "Update");
        assert_ne!(access_denied_msg(false), access_denied_msg(true));
    }

    fn hidden_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .admin(FieldAdmin::builder().hidden(true).build())
            .build()
    }

    fn visible_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    #[test]
    fn collects_only_hidden_fields_present_in_form_data() {
        let fields = vec![
            hidden_field("upload_id"),
            hidden_field("upload_url"), // hidden but absent from form data → skipped
            visible_field("title"),     // present but not hidden → skipped
        ];
        let mut form_data = HashMap::new();
        form_data.insert("upload_id".to_string(), "u-123".to_string());
        form_data.insert("title".to_string(), "My post".to_string());

        let out = collect_upload_hidden_fields(&fields, &form_data);

        assert_eq!(out, json!([{ "name": "upload_id", "value": "u-123" }]));
    }

    #[test]
    fn empty_when_no_hidden_fields_match() {
        let fields = vec![visible_field("title")];
        let mut form_data = HashMap::new();
        form_data.insert("title".to_string(), "x".to_string());

        assert_eq!(collect_upload_hidden_fields(&fields, &form_data), json!([]));
    }
}
