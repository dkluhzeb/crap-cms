//! HTTP upload API: JSON endpoints for programmatic file uploads.
//!
//! Routes:
//! - `POST   /api/upload/{slug}`      — upload file + create document
//! - `PATCH  /api/upload/{slug}/{id}`  — replace file on existing document
//! - `DELETE /api/upload/{slug}/{id}`  — delete upload document + files

use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, patch, post},
};
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::admin::handlers::collections::forms::parse_multipart_form;
use crate::admin::server::load_auth_user;
use crate::core::auth::{self, AuthUser};
use crate::core::event::EventUser;
use crate::core::upload::{self, inject_upload_metadata};
use crate::db::query::{self, AccessResult};

/// Build the upload API router with all routes.
#[cfg(not(tarpaulin_include))]
pub fn upload_router(state: AdminState) -> Router<AdminState> {
    Router::new()
        .route("/upload/{slug}", post(create_upload))
        .route("/upload/{slug}/{id}", patch(update_upload))
        .route("/upload/{slug}/{id}", delete(delete_upload))
        .with_state(state)
}

/// Extract an authenticated user from the `Authorization: Bearer <jwt>` header.
#[cfg(not(tarpaulin_include))]
fn extract_bearer_user(state: &AdminState, headers: &HeaderMap) -> Option<AuthUser> {
    let auth_header = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = auth_header.strip_prefix("Bearer ")?;
    let claims = auth::validate_token(token, &state.jwt_secret).ok()?;
    load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale)
}

/// Return a JSON error response.
#[cfg(not(tarpaulin_include))]
fn json_error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "error": message });
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// Return a JSON success response with the given status and body.
#[cfg(not(tarpaulin_include))]
fn json_ok(status: StatusCode, body: &serde_json::Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// POST /api/upload/{slug} — upload a file and create a document.
#[cfg(not(tarpaulin_include))]
async fn create_upload(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> Response {
    let auth_user = extract_bearer_user(&state, &headers);

    // Look up collection definition
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return json_error(
                StatusCode::NOT_FOUND,
                &format!("Collection '{}' not found", slug),
            );
        }
    };

    if !def.is_upload_collection() {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("Collection '{}' is not an upload collection", slug),
        );
    }

    // Check create access
    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
    let access = {
        let mut conn = match state.pool.get() {
            Ok(c) => c,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
        };
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
        };
        let result =
            state
                .hook_runner
                .check_access(def.access.create.as_deref(), user_doc, None, None, &tx);
        // Read-only access check — commit result is irrelevant, rollback on drop is safe
        let _ = tx.commit();
        result
    };
    match access {
        Ok(AccessResult::Denied) => {
            return json_error(StatusCode::FORBIDDEN, "Create access denied");
        }
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Access check error: {}", e),
            );
        }
        _ => {}
    }

    // Parse multipart form
    let (mut form_data, file) = match parse_multipart_form(request, &state).await {
        Ok(result) => result,
        Err(e) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                &format!("Multipart parse error: {}", e),
            );
        }
    };

    // File is required for upload creation
    let file = match file {
        Some(f) => f,
        None => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "No file provided (use field name '_file')",
            );
        }
    };

    // Process the upload (validate, save to disk, generate sizes) — runs on blocking thread
    let upload_config = match def.upload.as_ref() {
        Some(c) => c.clone(),
        None => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Upload config missing"),
    };
    let config_dir = state.config_dir.clone();
    let slug_for_upload = slug.clone();
    let global_max = state.config.upload.max_file_size;
    let processed = match tokio::task::spawn_blocking(move || {
        upload::process_upload(
            file,
            &upload_config,
            &config_dir,
            &slug_for_upload,
            global_max,
        )
    })
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return json_error(StatusCode::BAD_REQUEST, &e.to_string()),
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Task error: {}", e),
            );
        }
    };
    let queued_conversions = processed.queued_conversions.clone();
    let created_files = processed.created_files.clone();
    inject_upload_metadata(&mut form_data, &processed);

    // Strip field-level create-denied fields
    {
        if let Ok(mut conn) = state.pool.get()
            && let Ok(tx) = conn.transaction()
        {
            let denied =
                state
                    .hook_runner
                    .check_field_write_access(&def.fields, user_doc, "create", &tx);
            // Read-only access check — commit result is irrelevant, rollback on drop is safe
            let _ = tx.commit();
            for name in &denied {
                form_data.remove(name);
            }
        }
    }

    // Extract password for auth collections (unlikely for upload collections, but consistent)
    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };

    // Extract join table data
    let join_data = crate::admin::handlers::collections::forms::extract_join_data_from_form(
        &form_data,
        &def.fields,
    );

    // Extract draft flag
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let user_doc_owned = auth_user.as_ref().map(|au| au.user_doc.clone());
    let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
    let result = tokio::task::spawn_blocking(move || {
        crate::service::create_document(
            &pool,
            &runner,
            &slug_owned,
            &def_owned,
            crate::service::WriteInput {
                data: form_data,
                join_data: &join_data,
                password: password.as_deref(),
                locale_ctx: None,
                locale: None,
                draft,
                ui_locale,
            },
            user_doc_owned.as_ref(),
        )
    })
    .await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            // Enqueue deferred image conversions if any
            if !queued_conversions.is_empty()
                && let Ok(conn) = state.pool.get()
                && let Err(e) =
                    upload::enqueue_conversions(&conn, &slug, &doc.id, &queued_conversions)
            {
                tracing::warn!("Failed to enqueue image conversions: {}", e);
            }

            let edited_by = auth_user
                .as_ref()
                .map(|au| EventUser::new(au.claims.sub.clone(), au.claims.email.clone()));
            state.hook_runner.publish_event(
                &state.event_bus,
                &def.hooks,
                def.live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Create,
                slug,
                doc.id.clone(),
                doc.fields.clone(),
                edited_by,
            );

            let body = serde_json::json!({ "document": doc });
            json_ok(StatusCode::CREATED, &body)
        }
        Ok(Err(e)) => {
            cleanup_files(&created_files);
            json_error(StatusCode::BAD_REQUEST, &e.to_string())
        }
        Err(e) => {
            cleanup_files(&created_files);
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Task error: {}", e),
            )
        }
    }
}

/// PATCH /api/upload/{slug}/{id} — replace file on an existing document.
#[cfg(not(tarpaulin_include))]
async fn update_upload(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> Response {
    let auth_user = extract_bearer_user(&state, &headers);

    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return json_error(
                StatusCode::NOT_FOUND,
                &format!("Collection '{}' not found", slug),
            );
        }
    };

    if !def.is_upload_collection() {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("Collection '{}' is not an upload collection", slug),
        );
    }

    // Check update access
    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
    let access = {
        let mut conn = match state.pool.get() {
            Ok(c) => c,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
        };
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
        };
        let result = state.hook_runner.check_access(
            def.access.update.as_deref(),
            user_doc,
            Some(&id),
            None,
            &tx,
        );
        // Read-only access check — commit result is irrelevant, rollback on drop is safe
        let _ = tx.commit();
        result
    };
    match access {
        Ok(AccessResult::Denied) => {
            return json_error(StatusCode::FORBIDDEN, "Update access denied");
        }
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Access check error: {}", e),
            );
        }
        _ => {}
    }

    // Parse multipart form
    let (mut form_data, file) = match parse_multipart_form(request, &state).await {
        Ok(result) => result,
        Err(e) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                &format!("Multipart parse error: {}", e),
            );
        }
    };

    // Load old document to get file paths for cleanup
    let mut old_doc_fields: Option<HashMap<String, serde_json::Value>> = None;
    if let Some(ref f) = file
        && !f.data.is_empty()
        && let Ok(conn) = state.pool.get()
        && let Ok(Some(old_doc)) = query::find_by_id(&conn, &slug, &def, &id, None)
    {
        old_doc_fields = Some(old_doc.fields.clone());
    }

    // Process upload if a new file was provided — runs on blocking thread
    let mut queued_conversions = Vec::new();
    let mut created_files: Vec<std::path::PathBuf> = Vec::new();
    if let Some(f) = file
        && let Some(upload_config) = def.upload.clone()
    {
        let config_dir = state.config_dir.clone();
        let slug_for_upload = slug.clone();
        let global_max = state.config.upload.max_file_size;
        match tokio::task::spawn_blocking(move || {
            upload::process_upload(f, &upload_config, &config_dir, &slug_for_upload, global_max)
        })
        .await
        {
            Ok(Ok(processed)) => {
                queued_conversions = processed.queued_conversions.clone();
                created_files = processed.created_files.clone();
                inject_upload_metadata(&mut form_data, &processed);
            }
            Ok(Err(e)) => return json_error(StatusCode::BAD_REQUEST, &e.to_string()),
            Err(e) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Task error: {}", e),
                );
            }
        }
    }

    // Strip field-level update-denied fields
    {
        if let Ok(mut conn) = state.pool.get()
            && let Ok(tx) = conn.transaction()
        {
            let denied =
                state
                    .hook_runner
                    .check_field_write_access(&def.fields, user_doc, "update", &tx);
            // Read-only access check — commit result is irrelevant, rollback on drop is safe
            let _ = tx.commit();
            for name in &denied {
                form_data.remove(name);
            }
        }
    }

    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };

    let join_data = crate::admin::handlers::collections::forms::extract_join_data_from_form(
        &form_data,
        &def.fields,
    );

    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let def_owned = def.clone();
    let user_doc_owned = auth_user.as_ref().map(|au| au.user_doc.clone());
    let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
    let result = tokio::task::spawn_blocking(move || {
        crate::service::update_document(
            &pool,
            &runner,
            &slug_owned,
            &id_owned,
            &def_owned,
            crate::service::WriteInput {
                data: form_data,
                join_data: &join_data,
                password: password.as_deref(),
                locale_ctx: None,
                locale: None,
                draft,
                ui_locale,
            },
            user_doc_owned.as_ref(),
        )
    })
    .await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            // Clean up old files on success
            if let Some(old_fields) = old_doc_fields {
                upload::delete_upload_files(&state.config_dir, &old_fields);
            }

            // Enqueue deferred image conversions if any
            if !queued_conversions.is_empty()
                && let Ok(conn) = state.pool.get()
                && let Err(e) = upload::enqueue_conversions(&conn, &slug, &id, &queued_conversions)
            {
                tracing::warn!("Failed to enqueue image conversions: {}", e);
            }

            let edited_by = auth_user
                .as_ref()
                .map(|au| EventUser::new(au.claims.sub.clone(), au.claims.email.clone()));
            state.hook_runner.publish_event(
                &state.event_bus,
                &def.hooks,
                def.live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Update,
                slug,
                id,
                doc.fields.clone(),
                edited_by,
            );

            let body = serde_json::json!({ "document": doc });
            json_ok(StatusCode::OK, &body)
        }
        Ok(Err(e)) => {
            cleanup_files(&created_files);
            json_error(StatusCode::BAD_REQUEST, &e.to_string())
        }
        Err(e) => {
            cleanup_files(&created_files);
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Task error: {}", e),
            )
        }
    }
}

/// Delete a list of files, ignoring errors (best-effort orphan cleanup).
fn cleanup_files(files: &[std::path::PathBuf]) {
    for path in files {
        let _ = std::fs::remove_file(path);
    }
}

/// DELETE /api/upload/{slug}/{id} — delete an upload document and its files.
#[cfg(not(tarpaulin_include))]
async fn delete_upload(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let auth_user = extract_bearer_user(&state, &headers);

    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return json_error(
                StatusCode::NOT_FOUND,
                &format!("Collection '{}' not found", slug),
            );
        }
    };

    if !def.is_upload_collection() {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("Collection '{}' is not an upload collection", slug),
        );
    }

    // Check delete access
    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
    let access = {
        let mut conn = match state.pool.get() {
            Ok(c) => c,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
        };
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
        };
        let result = state.hook_runner.check_access(
            def.access.delete.as_deref(),
            user_doc,
            Some(&id),
            None,
            &tx,
        );
        // Read-only access check — commit result is irrelevant, rollback on drop is safe
        let _ = tx.commit();
        result
    };
    match access {
        Ok(AccessResult::Denied) => {
            return json_error(StatusCode::FORBIDDEN, "Delete access denied");
        }
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Access check error: {}", e),
            );
        }
        _ => {}
    }

    // Verify document exists before attempting delete
    let doc_exists = state
        .pool
        .get()
        .ok()
        .and_then(|conn| {
            query::find_by_id(&conn, &slug, &def, &id, None)
                .ok()
                .flatten()
        })
        .is_some();

    if !doc_exists {
        return json_error(
            StatusCode::NOT_FOUND,
            &format!("Document '{}' not found", id),
        );
    }

    // Before hooks + delete + upload cleanup in a single transaction
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let def_clone = def.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let user_doc_owned = auth_user.as_ref().map(|au| au.user_doc.clone());
    let config_dir = state.config_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::service::delete_document(
            &pool,
            &runner,
            &slug_owned,
            &id_owned,
            &def_clone,
            user_doc_owned.as_ref(),
            Some(&config_dir),
        )
    })
    .await;

    match result {
        Ok(Ok(_req_context)) => {
            let edited_by = auth_user
                .as_ref()
                .map(|au| EventUser::new(au.claims.sub.clone(), au.claims.email.clone()));
            state.hook_runner.publish_event(
                &state.event_bus,
                &def.hooks,
                def.live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Delete,
                slug,
                id,
                HashMap::new(),
                edited_by,
            );

            json_ok(StatusCode::OK, &serde_json::json!({ "success": true }))
        }
        Ok(Err(e)) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Delete error: {}", e),
        ),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task error: {}", e),
        ),
    }
}
