//! Form error rendering — unified handling for upload errors and validation errors.

use std::collections::HashMap;

use axum::{Extension, response::Response};
use serde_json::{Value, json};

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, CollectionContext, CollectionPermissions, DocumentRef, PageMeta,
            PageType,
            field::FieldContext,
            page::{RevisionConflictNotice, collections::CollectionFormErrorPage},
        },
        handlers::{
            forms::FormData,
            shared::{
                EnrichOptions, ErrorLabels, FormReadDenials, HxNav, PageRequest,
                apply_display_conditions, build_field_contexts, collection_read_denials,
                conflict_keeps_form, editor_read_ctx, enrich_field_contexts, forbidden,
                form_condition_data, get_user_doc, is_non_default_locale, page_with_toast, paths,
                readable_form_fields, redirect_response, split_sidebar_fields,
                translate_validation_errors, ui_locale_of, unpublish_conflict_response,
                write_error_response,
            },
        },
    },
    core::{AuthUser, Builder, CollectionDefinition, FieldDefinition, ValidationError},
    db::LocaleContext,
    hooks::ConditionContext,
    service::{RevisionConflict, ServiceError},
};

use super::{locked_field, password_field};

/// The hidden field values the submitted form actually carried, so the error
/// re-render can put them back as hidden inputs.
///
/// On an upload collection that is the focal point (`focal_x` / `focal_y`): the
/// edit page renders those inside the file-preview block, which the slim error
/// page does not carry, so without this round-trip a failed save resets the
/// focal point the user just moved. The server-derived columns (`url`,
/// `{size}_url`, `filesize`, …) are hidden fields too, but no form renders them
/// as inputs and every write strips them from user data, so they are simply
/// absent from `form_data` and nothing is collected for them.
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

/// The meta inputs a write handler takes out of the raw form before the write.
///
/// They are not document data, so they never reach the write's `DocumentFields`
/// — but the form did render them, and an error re-render has to put them back:
/// without `_locale` the corrected save writes a translation into the default
/// locale's columns, without `_locked` the update reads the missing box as
/// an explicit unlock, and without `_revision` the corrected save would no
/// longer be checked against the revision the editor loaded.
#[derive(Debug, Clone, Copy, Default, Builder)]
pub(in crate::admin::handlers::collections) struct SubmittedMeta<'a> {
    /// The content locale the form was submitted in (`_locale`).
    pub locale: Option<&'a str>,
    /// The lock box as the user left it — auth collections, edit only.
    pub locked: Option<bool>,
    /// The document revision the form was loaded at (`_revision`) — edit only.
    pub revision: Option<i64>,
    /// The action the form was submitted with (`_action`), which the
    /// revision-conflict notice's overwrite button submits again.
    pub action: Option<&'a str>,
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
    pub meta: SubmittedMeta<'a>,
    /// How the submit was issued — an htmx form post targeting `#main` gets
    /// the fragment back, not a second full document.
    pub hx: HxNav,
    /// Set when the save was refused because the document changed since the
    /// form was loaded.
    pub conflict: Option<RevisionConflictNotice>,
}

/// Re-add the auth-collection inputs the write handler took out of the form.
/// They come from the same constructors the create and edit forms use, so the
/// re-rendered form offers exactly the inputs the user submitted from — each
/// carrying its own field error (a password the policy refused), which the
/// declared-field builders never see because no schema declares these inputs.
fn append_auth_fields(
    fields: &mut Vec<FieldContext>,
    editing: bool,
    meta: SubmittedMeta<'_>,
    errors: &HashMap<String, String>,
) {
    let mut auth = vec![password_field(!editing)];

    if editing {
        auth.push(locked_field(meta.locked.unwrap_or(false)));
    }

    for field in &mut auth {
        let base = field.base_mut();
        base.error = errors.get(&base.name).cloned();
    }

    fields.extend(auth);
}

/// Build, enrich, condition and split the field contexts for the re-render,
/// in the locale the form was submitted in. The fields the viewer may not read
/// (`denied`) get no input, exactly as on the edit form — in each row, what the
/// viewer may not read in that row.
fn prepare_error_fields(
    p: &FormErrorParams<'_>,
    locale_ctx: Option<&LocaleContext>,
    non_default_locale: bool,
    denied: &FormReadDenials,
) -> (Vec<FieldContext>, Vec<FieldContext>) {
    let form_fields = readable_form_fields(&p.def.fields, &denied.flat);

    let mut fields = build_field_contexts(
        &form_fields,
        p.form.raw(),
        p.error_map,
        true,
        non_default_locale,
    );

    let enrich_opts = EnrichOptions::builder(p.error_map)
        .filter_hidden(true)
        .non_default_locale(non_default_locale)
        .doc_id(p.doc_id)
        .user(get_user_doc(p.auth_user))
        .locale_ctx(locale_ctx);

    enrich_field_contexts(
        &mut fields,
        &form_fields,
        p.form.join(),
        p.state,
        &enrich_opts.build(),
    );

    denied.rows.prune(&mut fields);

    let cond_ctx = ConditionContext {
        collection: &p.def.slug,
        operation: if p.doc_id.is_some() {
            "update"
        } else {
            "create"
        },
        user: get_user_doc(p.auth_user),
        ui_locale: p.auth_user.map(|Extension(au)| au.ui_locale.as_str()),
        locale: p.meta.locale,
        options: None,
    };

    // The submitted values, decoded like the write they were meant for — the
    // same data view the edit and create forms condition on.
    apply_display_conditions(
        &mut fields,
        &form_fields,
        &form_condition_data(&p.def.fields, p.form),
        &p.state.infra.hook_runner,
        true,
        &cond_ctx,
    );

    if p.def.is_auth_collection() {
        append_auth_fields(&mut fields, p.doc_id.is_some(), p.meta, p.error_map);
    }

    split_sidebar_fields(fields)
}

/// Build and render the form with an error toast. Handles both create (`doc_id = None`)
/// and edit (`doc_id = Some(id)`) modes, including upload hidden field preservation.
pub(in crate::admin::handlers::collections) async fn render_form_with_error(
    p: &FormErrorParams<'_>,
) -> Response {
    // The re-render stays in the locale the form was submitted in: the read
    // context for relationship labels comes from the same call the success-path
    // forms make, `with_editor_locale` below re-renders the picker in that same
    // locale, and `non_default_locale` re-locks the shared fields the editor may
    // not change from a translation.
    let locale_ctx = editor_read_ctx(p.state, p.meta.locale);
    let non_default_locale = is_non_default_locale(p.state, p.meta.locale);

    let denied = match p.doc_id {
        Some(id) => collection_read_denials(p.state, p.def, id, p.auth_user, p.meta.locale).await,
        None => FormReadDenials::default(),
    };

    let (main_fields, sidebar_fields) =
        prepare_error_fields(p, locale_ctx.as_ref(), non_default_locale, &denied);

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
    )
    .with_editor_locale(p.meta.locale, p.state);

    // The file is stored inside the write's own blocking task, working on its
    // own copy of the form, so a failed write leaves no injected metadata here
    // — the user re-picks the file either way. What this preserves is the
    // hidden inputs the submitted form rendered, i.e. the edit page's focal
    // point. Not gated on `editing` because a create carries none of them: the
    // collection is empty rather than wrong.
    let upload_hidden_fields = p.def.is_upload_collection().then(|| {
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
        unsaved: true,
        upload_hidden_fields,
        revision: p.meta.revision,
        revision_conflict: p.conflict.clone(),
    };

    page_with_toast(
        p.state,
        PageRequest::new(p.hx, p.auth_user),
        "collections/edit",
        &ctx,
        p.toast_msg,
    )
    .await
}

/// The rejected file's own message, when the write failed on the file rather
/// than on a form field.
///
/// The upload service reports a rejected file (wrong type, too large,
/// undecodable image) as a `_file` validation error. `_file` is the multipart
/// part name, not a field the form renders, so its message has nowhere to
/// appear inline — it becomes the toast, which is what the user needs to read.
fn file_error_message(ve: &ValidationError) -> Option<&str> {
    ve.errors
        .iter()
        .find(|e| e.field == "_file")
        .map(|e| e.message.as_str())
}

/// What the validation re-render needs beyond the errors themselves.
struct ValidationRender<'a> {
    state: &'a AdminState,
    def: &'a CollectionDefinition,
    form: &'a FormData,
    doc_id: Option<&'a str>,
    auth_user: Option<&'a Extension<AuthUser>>,
    meta: SubmittedMeta<'a>,
    hx: HxNav,
}

/// Re-render the form with validation errors (works for both create and edit).
async fn render_form_validation_errors(p: &ValidationRender<'_>, ve: &ValidationError) -> Response {
    let locale = ui_locale_of(p.auth_user);

    let labels = ErrorLabels::new(&p.def.fields, Some(p.form.raw()));
    let error_map = translate_validation_errors(ve, &labels, &p.state.translations, locale);

    let toast_msg = file_error_message(ve)
        .unwrap_or_else(|| p.state.translations.get(locale, "validation.error_summary"));

    render_form_with_error(&FormErrorParams {
        state: p.state,
        def: p.def,
        form: p.form,
        error_map: &error_map,
        doc_id: p.doc_id,
        auth_user: p.auth_user,
        toast_msg,
        meta: p.meta,
        hx: p.hx,
        conflict: None,
    })
    .await
}

/// Re-render the edit form after its save was refused by a revision conflict.
///
/// The editor's unsaved values stay in the form, which now carries the
/// document's current revision: the notice offers a reload (the saved
/// document, discarding the edits) or an overwrite, which resubmits the form
/// with the same action at that revision.
async fn render_revision_conflict(
    p: &WriteErrorParams<'_>,
    conflict: RevisionConflict,
) -> Response {
    let locale = ui_locale_of(p.auth_user);
    let id = p.doc_id.unwrap_or_default();

    let notice = RevisionConflictNotice::new(
        paths::collection_item(&p.def.slug, id),
        p.meta.action.unwrap_or_default(),
    );

    let meta = SubmittedMeta {
        revision: Some(conflict.current),
        ..p.meta
    };

    render_form_with_error(&FormErrorParams {
        state: p.state,
        def: p.def,
        form: p.form,
        error_map: &HashMap::new(),
        doc_id: p.doc_id,
        auth_user: p.auth_user,
        toast_msg: p.state.translations.get(locale, "revision_conflict_title"),
        meta,
        hx: p.hx,
        conflict: Some(notice),
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
    /// The meta inputs the handler took out of the form — see [`SubmittedMeta`].
    pub meta: SubmittedMeta<'a>,
    /// How the submit was issued — see [`FormErrorParams::hx`].
    pub hx: HxNav,
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
    /// The edited document was saved by someone else after the form was
    /// loaded — re-render with the conflict notice.
    Conflict,
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
        ServiceError::Conflict(_) if editing => WriteErrorResponse::Conflict,
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
                let render = ValidationRender {
                    state: p.state,
                    def: p.def,
                    form: p.form,
                    doc_id: p.doc_id,
                    auth_user: p.auth_user,
                    meta: p.meta,
                    hx: p.hx,
                };

                return render_form_validation_errors(&render, ve).await;
            }

            write_error_response(p.state, ui_locale_of(p.auth_user), op_label(editing), p.err)
        }
        WriteErrorResponse::Conflict => {
            if !conflict_keeps_form(p.meta.action.unwrap_or_default()) {
                return unpublish_conflict_response(p.state, ui_locale_of(p.auth_user));
            }

            if let ServiceError::Conflict(conflict) = p.err {
                return render_revision_conflict(&p, conflict).await;
            }

            write_error_response(p.state, ui_locale_of(p.auth_user), op_label(editing), p.err)
        }
        WriteErrorResponse::Toast => {
            write_error_response(p.state, ui_locale_of(p.auth_user), op_label(editing), p.err)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::{
        FieldError,
        field::{FieldAdmin, FieldType},
    };

    use super::*;

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

    /// A revision conflict re-renders the edit form with the conflict notice;
    /// a create has no revision to conflict on, so one would be a real error.
    #[test]
    fn a_revision_conflict_re_renders_only_an_edit() {
        let conflict = || ServiceError::Conflict(RevisionConflict::new(1, 2));

        assert_eq!(
            classify_write_error(true, &conflict()),
            WriteErrorResponse::Conflict
        );
        assert_eq!(
            classify_write_error(false, &conflict()),
            WriteErrorResponse::Toast
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

    /// A rejected file has no form field to render its message against, so it
    /// becomes the toast — the same message the dedicated upload-error page
    /// used to show before the file lifecycle moved into the write.
    #[test]
    fn a_rejected_file_becomes_the_toast() {
        let ve = ValidationError::new(vec![FieldError::new(
            "_file",
            "File type 'application/zip' is not allowed",
        )]);

        assert_eq!(
            file_error_message(&ve),
            Some("File type 'application/zip' is not allowed")
        );
    }

    /// An ordinary field error renders inline, so the toast stays the generic
    /// validation summary.
    #[test]
    fn a_field_error_leaves_the_toast_alone() {
        let ve = ValidationError::new(vec![FieldError::new("title", "required")]);

        assert_eq!(file_error_message(&ve), None);
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

    /// Regression: the auth inputs are not field definitions, so the error
    /// re-render used to drop them. Without the password box the user cannot
    /// set one, and without `_locked` the corrected save posts no lock box —
    /// which the update path reads as an explicit unlock.
    #[test]
    fn the_error_form_re_adds_the_auth_inputs() {
        let mut fields = Vec::new();
        append_auth_fields(
            &mut fields,
            true,
            SubmittedMeta::builder()
                .locale(Some("de"))
                .locked(Some(true))
                .build(),
            &HashMap::new(),
        );

        let names: Vec<&str> = fields.iter().map(|f| f.base().name.as_str()).collect();
        assert_eq!(names, vec!["password", "_locked"]);

        let FieldContext::Checkbox(locked) = &fields[1] else {
            panic!("expected the lock checkbox")
        };
        assert!(
            locked.checked,
            "the lock box comes back as the user left it"
        );
    }

    /// Regression: the password box was rebuilt without its field error, so a
    /// password the policy refused re-rendered the form with no message at all.
    #[test]
    fn the_re_added_password_box_shows_its_error() {
        let errors = HashMap::from([("password".to_string(), "zu kurz".to_string())]);
        let mut fields = Vec::new();
        append_auth_fields(&mut fields, false, SubmittedMeta::default(), &errors);

        assert_eq!(fields[0].base().error.as_deref(), Some("zu kurz"));
    }

    /// On create there is no lock box — the create form has none either.
    #[test]
    fn the_create_error_form_adds_only_the_password_box() {
        let mut fields = Vec::new();
        append_auth_fields(
            &mut fields,
            false,
            SubmittedMeta::default(),
            &HashMap::new(),
        );

        let names: Vec<&str> = fields.iter().map(|f| f.base().name.as_str()).collect();
        assert_eq!(names, vec!["password"]);
        assert!(fields[0].base().required, "a create needs a password");
    }

    /// An unchecked lock box submits nothing, so the handler passes `false` —
    /// and an absent flag must never come back as locked.
    #[test]
    fn an_unsubmitted_lock_box_comes_back_unchecked() {
        for locked in [None, Some(false)] {
            let mut fields = Vec::new();
            append_auth_fields(
                &mut fields,
                true,
                SubmittedMeta::builder().locked(locked).build(),
                &HashMap::new(),
            );

            let FieldContext::Checkbox(box_) = &fields[1] else {
                panic!("expected the lock checkbox")
            };
            assert!(!box_.checked, "{locked:?}");
        }
    }

    /// What an upload EDIT re-render actually preserves: the focal point, the
    /// only hidden upload input a form renders. The server-derived columns are
    /// hidden fields too, but no form renders them and every write strips them
    /// from user data, so they never reach the submitted form.
    #[test]
    fn an_upload_edit_preserves_the_focal_point() {
        let fields = vec![
            hidden_field("url"),
            hidden_field("filesize"),
            hidden_field("focal_x"),
            hidden_field("focal_y"),
        ];

        let mut form_data = HashMap::new();
        form_data.insert("focal_x".to_string(), "0.25".to_string());
        form_data.insert("focal_y".to_string(), "0.75".to_string());

        let out = collect_upload_hidden_fields(&fields, &form_data);

        assert_eq!(
            out,
            json!([
                { "name": "focal_x", "value": "0.25" },
                { "name": "focal_y", "value": "0.75" },
            ])
        );
    }

    #[test]
    fn empty_when_no_hidden_fields_match() {
        let fields = vec![visible_field("title")];
        let mut form_data = HashMap::new();
        form_data.insert("title".to_string(), "x".to_string());

        assert_eq!(collect_upload_hidden_fields(&fields, &form_data), json!([]));
    }
}
