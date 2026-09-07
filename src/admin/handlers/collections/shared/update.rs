//! Update handler — processes form submissions for editing collection items.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use axum::{
    Extension,
    response::{IntoResponse, Response},
};
use tokio::task;
use tracing::{error, warn};

use crate::{
    admin::{
        AdminState,
        handlers::{
            forms::FormData,
            shared::{
                get_user_doc, htmx_redirect, parse_request_locale, paths, redirect_response,
                toast_only_error,
            },
        },
    },
    core::{
        AuthUser, CollectionDefinition, Document, DocumentFields, ReqContext, flatten_group_fields,
        upload::{UploadedFile, delete_upload_files, enqueue_conversions},
    },
    db::{LocaleContext, query::locale_locked_field_names},
    service::{
        self, AppInfra, ServiceContext, ServiceError,
        auth::{AccountAction, perform_account_action},
        op::{Operation, Unpublish, UnpublishArgs, Update, UpdateArgs},
    },
};

use super::upload::{UploadParams, UploadResult, process_collection_upload};
use super::{WriteErrorParams, handle_collection_write_error};

/// Handle post-update success: commit upload, clean old files, enqueue conversions.
fn handle_update_success(state: &AdminState, slug: &str, id: &str, upload: Option<UploadResult>) {
    if let Some(mut ur) = upload {
        ur.guard.commit();

        if let Some(old_fields) = ur.old_doc_fields
            && let Some(def) = state.infra.registry.get_collection(slug)
            && let Some(upload) = def.upload.as_ref()
        {
            delete_upload_files(&*state.infra.storage, &old_fields, upload);
        }

        if !ur.queued_conversions.is_empty()
            && let Ok(conn) = state.infra.pool.get()
            && let Err(e) = enqueue_conversions(
                &conn,
                slug,
                id,
                &ur.queued_conversions,
                state.config.jobs.system_image_max_attempts(),
            )
        {
            warn!("Failed to enqueue image conversions: {}", e);
        }
    }
}

/// Whether (and how) to update the auth collection's `_locked` flag.
///
/// Auth collections render a `_locked` checkbox; non-auth collections never
/// touch the lock state. The three states are: skip the update entirely
/// (non-auth), lock the account, or unlock it.
enum LockUpdate {
    Skip,
    Set(bool),
}

/// Prepared update input.
struct UpdateInput {
    form: FormData,
    password: Option<String>,
    lock: LockUpdate,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    action: String,
    /// A file was processed and server-derived metadata injected into `form`.
    trusted_upload: bool,
}

/// Owned bundle for the spawn-blocking update body. Process-stable dependencies
/// come from the shared [`AppInfra`]; the rest is per-call.
struct UpdateBlockingInput {
    infra: Arc<AppInfra>,
    slug: String,
    id: String,
    def: CollectionDefinition,
    user_doc: Option<Document>,
    ui_locale: Option<String>,
    input: UpdateInput,
}

/// Strip shared (locale-locked) fields from a non-default-locale PUBLISH.
///
/// The admin edit form submits shared (non-localized) fields as read-only
/// display artifacts — including the hidden row-id / `_block_type` inputs of a
/// shared array/blocks field. Under a non-default locale the service rejects a
/// write that carries them, to stop a programmatic caller (gRPC/Lua/MCP) from
/// silently overwriting the canonical default-locale value. `save_draft` already
/// drops them on the draft path; this does the same on publish so saving a
/// translation isn't rejected, while the service guard still protects the
/// programmatic surfaces. No-op for the default locale, drafts, or when nothing
/// is locale-locked.
fn strip_locale_locked_for_publish(
    data: DocumentFields,
    def: &CollectionDefinition,
    locale_ctx: Option<&LocaleContext>,
    draft: bool,
) -> DocumentFields {
    if draft {
        return data;
    }

    let locked = locale_locked_field_names(&def.fields, locale_ctx);
    if locked.is_empty() {
        return data;
    }

    // Flatten groups to `group__sub` so the locked-name filter matches the shape
    // `locale_locked_field_names` produces; the write path re-nests.
    flatten_group_fields(&data, &def.fields)
        .into_iter()
        .filter(|(k, _)| !locked.contains(k))
        .collect()
}

/// Synchronous body of [`spawn_update`]. Builds the service context, runs
/// either `unpublish_document` (for `action == "unpublish"` on versioned
/// collections) or `update_document`, and applies the optional account-lock
/// toggle for auth collections.
fn update_document_blocking(
    args: UpdateBlockingInput,
) -> Result<service::WriteResult, ServiceError> {
    let ctx = service::ServiceContext::collection(&args.slug, &args.def)
        .infra(&args.infra)
        .user(args.user_doc.as_ref())
        .ui_locale(args.ui_locale)
        .build();

    // Route an unpublish action to the unpublish path regardless of versioning:
    // the shared service gate rejects unpublish on a non-versioned collection
    // (an explicit error) rather than silently doing a normal update, matching
    // the Lua surface. Both branches run the shared operation bodies.
    let result = if args.input.action == "unpublish" {
        let doc = Unpublish::run(&ctx, UnpublishArgs::new(args.id.as_str()))?;

        Ok((doc, ReqContext::new()))
    } else {
        let data = strip_locale_locked_for_publish(
            args.input.form.into(),
            &args.def,
            args.input.locale_ctx.as_ref(),
            args.input.draft,
        );

        let op_args = UpdateArgs::builder(args.id.as_str(), data)
            .password(args.input.password)
            .locale_ctx(args.input.locale_ctx)
            .draft(args.input.draft)
            .trusted_upload_metadata(args.input.trusted_upload)
            .build();

        Update::run(&ctx, op_args)
    };

    if result.is_ok()
        && let LockUpdate::Set(should_lock) = args.input.lock
    {
        let conn = args
            .infra
            .pool
            .get()
            .context("DB connection for lock update")?;
        // Gate the lock toggle through `perform_account_action` so the admin
        // surface honors `access.unlock` (`?? update`) against the target user —
        // identical to the gRPC LockAccount/UnlockAccount path. Building a
        // collection context (def + caller `user` + runner) is what lets the
        // access hook run; a `slug_only` context would silently skip the check.
        let ctx = ServiceContext::collection(&args.slug, &args.def)
            .conn(&conn)
            .runner(&args.infra.hook_runner)
            .user(args.user_doc.as_ref())
            .invalidation_transport(Some(args.infra.invalidation_transport.clone()))
            .build();

        let action = if should_lock {
            AccountAction::Lock
        } else {
            AccountAction::Unlock
        };
        perform_account_action(&ctx, &args.id, action)?;
    }

    result
}

/// Run the blocking update/unpublish + lock update task.
async fn spawn_update(
    state: &AdminState,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    auth_user: Option<&Extension<AuthUser>>,
    input: UpdateInput,
) -> Result<Result<service::WriteResult, ServiceError>, task::JoinError> {
    let ui_locale = auth_user.map(|Extension(au)| au.ui_locale.clone());
    // The unpublish branch reads the row via `find_by_id_raw`, which needs
    // a `LocaleContext` to emit `title__en`/`title__de` for localized
    // fields when locales are enabled. Threading the config through
    // `ServiceContext` lets the service build a default `All` context.
    let args = UpdateBlockingInput {
        infra: state.infra.clone(),
        slug: slug.to_string(),
        id: id.to_string(),
        def: def.clone(),
        user_doc: get_user_doc(auth_user).cloned(),
        ui_locale,
        input,
    };

    task::spawn_blocking(move || update_document_blocking(args)).await
}

/// Process a form update for a collection item (called from `update_action.rs`).
pub(in crate::admin::handlers::collections) async fn do_update(
    state: &AdminState,
    slug: &str,
    id: &str,
    form_data: HashMap<String, String>,
    file: Option<UploadedFile>,
    auth_user: Option<&Extension<AuthUser>>,
) -> Response {
    let Some(def) = state.infra.registry.get_collection(slug).cloned() else {
        return redirect_response(paths::COLLECTIONS_ROOT).into_response();
    };

    let mut form = FormData::from_raw(form_data, &def.fields);

    let action = form.take_action();
    let draft = action == "save_draft";
    let locale_ctx = match parse_request_locale(form.take_locale().as_deref(), &state.config.locale)
    {
        Ok(ctx) => ctx,
        Err(msg) => return toast_only_error(&msg),
    };

    let mut upload_result = None;

    if let Some(f) = file
        && def.upload.is_some()
    {
        match process_collection_upload(
            &UploadParams {
                state,
                def: &def,
                slug,
                doc_id: Some(id),
                locale_ctx: locale_ctx.as_ref(),
                auth_user,
            },
            form.raw_mut(),
            f,
        )
        .await
        {
            Ok(ur) => upload_result = Some(ur),
            Err(resp) => return resp.into_response(),
        }
    }

    // Field write access is now checked inside service::update_document_in_conn.

    let password = if def.is_auth_collection() {
        form.take("password")
    } else {
        None
    };

    let lock = if def.is_auth_collection() {
        let raw = form.take("_locked");
        let should_lock = matches!(raw.as_deref(), Some("on" | "1"));
        LockUpdate::Set(should_lock)
    } else {
        LockUpdate::Skip
    };

    if let Some(ref pw) = password
        && !pw.is_empty()
        && let Err(e) = state.config.auth.password_policy.validate(pw)
    {
        return toast_only_error(&e.to_string()).into_response();
    }

    let form_for_error = form.clone();

    let result = spawn_update(
        state,
        slug,
        id,
        &def,
        auth_user,
        UpdateInput {
            form,
            password,
            lock,
            locale_ctx,
            draft,
            action,
            trusted_upload: upload_result.is_some(),
        },
    )
    .await;

    match result {
        Ok(Ok(_)) => {
            handle_update_success(state, slug, id, upload_result);

            htmx_redirect(&paths::collection_item(slug, id))
        }
        Ok(Err(e)) => {
            handle_collection_write_error(WriteErrorParams {
                state,
                def: &def,
                form: &form_for_error,
                err: e,
                doc_id: Some(id),
                auth_user,
            })
            .await
        }
        Err(e) => {
            error!("Update task error: {}", e);
            redirect_response(&paths::collection_item(slug, id))
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::LocaleConfig;
    use crate::core::{FieldDefinition, FieldType};
    use crate::db::query::LocaleMode;

    fn def_with_shared_fields() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("slug", FieldType::Text).build(),
            FieldDefinition::builder("tags", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("name", FieldType::Text).build(),
                ])
                .build(),
        ];
        def
    }

    fn ctx(locale: &str) -> LocaleContext {
        LocaleContext {
            mode: LocaleMode::Single(locale.to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        }
    }

    /// Regression: publishing a non-default-locale edit strips the shared
    /// (locale-locked) fields the admin form submits read-only — including a
    /// shared array whose only submitted key is the new hidden row id — so the
    /// service's shared-field guard doesn't reject the whole save. The localized
    /// field is kept.
    #[test]
    fn publish_strips_shared_fields_under_non_default_locale() {
        let def = def_with_shared_fields();
        let data: DocumentFields = [
            ("title".to_string(), json!("Titel")),
            ("slug".to_string(), json!("neu")),
            ("tags".to_string(), json!([{ "id": "row1", "name": "x" }])),
        ]
        .into_iter()
        .collect();

        let out = strip_locale_locked_for_publish(data, &def, Some(&ctx("de")), false);
        assert!(out.contains_key("title"), "localized field is kept");
        assert!(!out.contains_key("slug"), "shared scalar is stripped");
        assert!(
            !out.contains_key("tags"),
            "shared array (hidden-id-only submit) is stripped, so publish isn't rejected"
        );
    }

    /// No-op off the non-default-locale publish path: default locale, drafts
    /// (`save_draft` strips), and a `None` locale context all pass data through.
    #[test]
    fn publish_strip_is_noop_off_the_non_default_publish_path() {
        let def = def_with_shared_fields();
        let shared =
            || -> DocumentFields { [("slug".to_string(), json!("neu"))].into_iter().collect() };

        assert!(
            strip_locale_locked_for_publish(shared(), &def, Some(&ctx("en")), false)
                .contains_key("slug"),
            "default locale is untouched"
        );
        assert!(
            strip_locale_locked_for_publish(shared(), &def, Some(&ctx("de")), true)
                .contains_key("slug"),
            "draft path is untouched (save_draft strips)"
        );
        assert!(
            strip_locale_locked_for_publish(shared(), &def, None, false).contains_key("slug"),
            "no locale context is untouched"
        );
    }
}
