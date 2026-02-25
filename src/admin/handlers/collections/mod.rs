//! Collection CRUD handlers: list, create, edit, delete.

pub(crate) mod forms;

use axum::{
    extract::{Form, FromRequest, Path, Query, State},
    response::IntoResponse,
    Extension,
};
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::core::auth::{AuthUser, Claims};
use crate::core::field::FieldType;
use crate::core::upload::{self, UploadedFile};
use crate::core::validate::ValidationError;
use crate::db::{ops, query};
use crate::db::query::{AccessResult, FindQuery, Filter, FilterOp, FilterClause, LocaleContext};
use crate::hooks::lifecycle::HookEvent;

use super::shared::{
    PaginationParams, LocaleParams,
    user_json, get_user_doc, strip_denied_fields,
    check_access_or_forbid, build_locale_template_data,
    is_non_default_locale,
    build_field_contexts, enrich_field_contexts,
    forbidden, redirect_response, html_with_toast,
    render_or_error, not_found, server_error,
};

use crate::core::upload::inject_upload_metadata;
use forms::{extract_join_data_from_form, parse_multipart_form};

/// GET /admin/collections — list all registered collections
pub async fn list_collections(
    State(state): State<AdminState>,
    claims: Option<Extension<Claims>>,
) -> impl IntoResponse {
    let mut collections = Vec::new();
    {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(e) => return server_error(&state, &format!("Registry lock poisoned: {}", e)).into_response(),
        };
        for (slug, def) in &reg.collections {
            collections.push(serde_json::json!({
                "slug": slug,
                "display_name": def.display_name(),
                "field_count": def.fields.len(),
            }));
        }
    }
    collections.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    let data = serde_json::json!({
        "title": "Collections",
        "collections": collections,
        "globals": state.sidebar_globals(),
        "user": user_json(&claims),
    });

    render_or_error(&state, "collections/list", &data).into_response()
}

/// GET /admin/collections/{slug} — list items in a collection
pub async fn list_items(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Query(params): Query<PaginationParams>,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(e) => return server_error(&state, &format!("Registry lock poisoned: {}", e)).into_response(),
        };
        match reg.get_collection(&slug) {
            Some(d) => d.clone(),
            None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
        }
    };

    // Check read access
    let access_result = match check_access_or_forbid(
        &state, def.access.read.as_deref(), &auth_user, None, None,
    ) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if matches!(access_result, AccessResult::Denied) {
        return forbidden(&state, "You don't have permission to view this collection").into_response();
    }

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(20).min(100);
    let offset = (page - 1) * per_page;
    let search = params.search.filter(|s| !s.trim().is_empty());

    // Build search filters (OR across searchable fields)
    let mut filters: Vec<FilterClause> = Vec::new();

    // Merge access constraint filters
    if let AccessResult::Constrained(ref constraint_filters) = access_result {
        filters.extend(constraint_filters.clone());
    }
    if let Some(ref search_term) = search {
        let searchable = if !def.admin.list_searchable_fields.is_empty() {
            def.admin.list_searchable_fields.clone()
        } else if let Some(ref title_field) = def.admin.use_as_title {
            vec![title_field.clone()]
        } else {
            // Fall back to all text-type fields
            def.fields.iter()
                .filter(|f| matches!(f.field_type, FieldType::Text | FieldType::Textarea))
                .map(|f| f.name.clone())
                .collect()
        };

        if !searchable.is_empty() {
            let or_filters: Vec<Filter> = searchable.iter()
                .map(|field| Filter {
                    field: field.clone(),
                    op: FilterOp::Contains(search_term.clone()),
                })
                .collect();
            filters.push(FilterClause::Or(
                or_filters.into_iter().map(|f| vec![f]).collect()
            ));
        }
    }

    let find_query = FindQuery {
        filters: filters.clone(),
        order_by: def.admin.default_sort.clone(),
        limit: Some(per_page),
        offset: Some(offset),
        select: None,
    };

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let fields = def.fields.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let read_result = tokio::task::spawn_blocking(move || {
        runner.fire_before_read(&hooks, &slug_owned, "find", HashMap::new())?;
        let total = ops::count_documents(&pool, &slug_owned, &def_owned, &filters, None)?;
        let mut docs = ops::find_documents(&pool, &slug_owned, &def_owned, &find_query, None)?;
        // Assemble sizes for upload collections
        if let Some(ref upload_config) = def_owned.upload {
            if upload_config.enabled {
                for doc in &mut docs {
                    upload::assemble_sizes_object(doc, upload_config);
                }
            }
        }
        let docs = runner.apply_after_read_many(&hooks, &fields, &slug_owned, "find", docs);
        Ok::<_, anyhow::Error>((docs, total))
    }).await;

    let (documents, total) = match read_result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return server_error(&state, &format!("Query error: {}", e)).into_response(),
        Err(e) => return server_error(&state, &format!("Task error: {}", e)).into_response(),
    };

    // Strip field-level read-denied fields from documents
    let denied_fields = {
        let user_doc = get_user_doc(&auth_user);
        let conn = match state.pool.get() {
            Ok(c) => c,
            Err(_) => return server_error(&state, "Database error").into_response(),
        };
        state.hook_runner.check_field_read_access(&def.fields, user_doc, &conn)
    };
    let documents: Vec<_> = documents.into_iter().map(|mut doc| {
        for field_name in &denied_fields {
            doc.fields.remove(field_name);
        }
        doc
    }).collect();

    let total_pages = ((total as f64) / (per_page as f64)).ceil() as i64;

    let title_field = def.title_field().map(|s| s.to_string());
    let is_upload = def.is_upload_collection();
    let admin_thumbnail = def.upload.as_ref()
        .and_then(|u| u.admin_thumbnail.as_ref().cloned());
    let items: Vec<_> = documents.iter().map(|doc| {
        let title_value = title_field.as_ref()
            .and_then(|f| doc.get_str(f))
            .unwrap_or_else(|| {
                // For upload collections, use filename as title
                if is_upload {
                    doc.get_str("filename").unwrap_or(&doc.id)
                } else {
                    &doc.id
                }
            });
        let status = doc.fields.get("_status")
            .and_then(|v| v.as_str())
            .unwrap_or("published");
        let mut item = serde_json::json!({
            "id": doc.id,
            "title_value": title_value,
            "created_at": doc.created_at,
            "updated_at": doc.updated_at,
            "status": status,
        });

        // Add thumbnail URL for upload collections
        if is_upload {
            let mime = doc.get_str("mime_type").unwrap_or("");
            if mime.starts_with("image/") {
                let thumb_url = admin_thumbnail.as_ref()
                    .and_then(|thumb_name| {
                        doc.fields.get("sizes")
                            .and_then(|v| v.get(thumb_name))
                            .and_then(|v| v.get("url"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .or_else(|| doc.get_str("url").map(|s| s.to_string()));
                if let Some(url) = thumb_url {
                    item["thumbnail_url"] = serde_json::json!(url);
                }
            }
        }

        item
    }).collect();

    // Build pagination URLs preserving search param
    let search_param = search.as_ref()
        .map(|s| {
            let encoded: String = s.bytes().map(|b| {
                if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
                    format!("{}", b as char)
                } else {
                    format!("%{:02X}", b)
                }
            }).collect();
            format!("&search={}", encoded)
        })
        .unwrap_or_default();

    let has_drafts = def.has_drafts();

    let data = serde_json::json!({
        "page_title": def.display_name(),
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
        "collection": {
            "slug": def.slug,
            "display_name": def.display_name(),
            "singular_name": def.singular_name(),
            "title_field": title_field,
        },
        "items": items,
        "has_drafts": has_drafts,
        "search": search,
        "page": page,
        "per_page": per_page,
        "total": total,
        "total_pages": total_pages,
        "has_pagination": total_pages > 1,
        "has_prev": page > 1,
        "has_next": page < total_pages,
        "prev_url": format!("/admin/collections/{}?page={}{}", slug, page - 1, search_param),
        "next_url": format!("/admin/collections/{}?page={}{}", slug, page + 1, search_param),
        "user": user_json(&claims),
    });

    render_or_error(&state, "collections/items", &data).into_response()
}

/// GET /admin/collections/{slug}/create — show create form
pub async fn create_form(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Query(locale_params): Query<LocaleParams>,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(e) => return server_error(&state, &format!("Registry lock poisoned: {}", e)).into_response(),
        };
        match reg.get_collection(&slug) {
            Some(d) => d.clone(),
            None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
        }
    };

    // Check create access
    match check_access_or_forbid(
        &state, def.access.create.as_deref(), &auth_user, None, None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to create items in this collection").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let non_default_locale = is_non_default_locale(&state, locale_params.locale.as_deref());
    let mut fields = build_field_contexts(&def.fields, &HashMap::new(), &HashMap::new(), true, non_default_locale);

    // Enrich relationship and array fields
    enrich_field_contexts(&mut fields, &def.fields, &HashMap::new(), &state, true, non_default_locale);

    if def.is_auth_collection() {
        fields.push(serde_json::json!({
            "name": "password",
            "field_type": "password",
            "label": "Password",
            "required": true,
            "value": "",
            "description": "Set the user's password",
        }));
    }

    let (_locale_ctx, locale_data) = build_locale_template_data(&state, locale_params.locale.as_deref());

    let mut data = serde_json::json!({
        "page_title": format!("Create {}", def.singular_name()),
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
        "collection": {
            "slug": def.slug,
            "display_name": def.display_name(),
            "singular_name": def.singular_name(),
        },
        "fields": fields,
        "editing": false,
        "has_drafts": def.has_drafts(),
        "user": user_json(&claims),
        "breadcrumbs": [
            { "label": "Collections", "url": "/admin/collections" },
            { "label": def.display_name(), "url": format!("/admin/collections/{}", slug) },
            { "label": format!("Create {}", def.singular_name()) },
        ],
    });

    // Merge locale data into template context
    if let Some(obj) = locale_data.as_object() {
        for (k, v) in obj {
            data[k] = v.clone();
        }
    }

    // Add upload context for upload collections
    if def.is_upload_collection() {
        data["is_upload"] = serde_json::json!(true);
        if let Some(ref u) = def.upload {
            if !u.mime_types.is_empty() {
                data["upload_accept"] = serde_json::json!(u.mime_types.join(","));
            }
        }
    }

    render_or_error(&state, "collections/edit", &data).into_response()
}

/// POST /admin/collections/{slug} — create a new item
pub async fn create_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(_) => return redirect_response("/admin/collections"),
        };
        reg.get_collection(&slug).cloned()
    };
    let def = match def {
        Some(d) => d,
        None => return redirect_response("/admin/collections"),
    };

    // Check create access
    match check_access_or_forbid(
        &state, def.access.create.as_deref(), &auth_user, None, None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to create items in this collection").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    // Parse form data — multipart for upload collections, regular form for others
    let (mut form_data, file) = if def.is_upload_collection() {
        match parse_multipart_form(request, &state).await {
            Ok(result) => result,
            Err(e) => {
                tracing::error!("Multipart parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/create", slug));
            }
        }
    } else {
        let Form(data) = match Form::<HashMap<String, String>>::from_request(request, &state).await {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Form parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/create", slug));
            }
        };
        (data, None)
    };

    // Process upload if file present
    if let Some(f) = file {
        if let Some(ref upload_config) = def.upload {
            match upload::process_upload(
                &f, upload_config, &state.config_dir, &slug,
                state.config.upload.max_file_size,
            ) {
                Ok(processed) => inject_upload_metadata(&mut form_data, &processed),
                Err(e) => {
                    tracing::error!("Upload processing error: {}", e);
                    let fields = build_field_contexts(&def.fields, &form_data, &HashMap::new(), true, false);
                    let data = serde_json::json!({
                        "page_title": format!("Create {}", def.singular_name()),
                        "collections": state.sidebar_collections(),
                        "globals": state.sidebar_globals(),
                        "collection": {
                            "slug": def.slug,
                            "display_name": def.display_name(),
                            "singular_name": def.singular_name(),
                        },
                        "fields": fields,
                        "editing": false,
                        "is_upload": true,
                    });
                    return html_with_toast(&state, "collections/edit", &data, &e.to_string());
                }
            }
        }
    }

    // Strip field-level create-denied fields
    {
        let user_doc = get_user_doc(&auth_user);
        if let Ok(conn) = state.pool.get() {
            let denied = state.hook_runner.check_field_write_access(&def.fields, user_doc, "create", &conn);
            for name in &denied {
                form_data.remove(name);
            }
        }
    }

    // Extract password before it enters hooks/regular data flow
    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };

    // Extract join table data (arrays + has-many relationships) from form
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    // Extract action (publish/save_draft) and locale before they enter hooks
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let form_locale = form_data.remove("_locale");
    let locale_ctx = LocaleContext::from_locale_string(
        form_locale.as_deref(), &state.config.locale,
    );

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let form_data_clone = form_data.clone();
    let user_doc = get_user_doc(&auth_user).cloned();
    let locale = locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        query::LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let result = tokio::task::spawn_blocking(move || {
        crate::service::create_document(
            &pool, &runner, &slug_owned, &def_owned,
            form_data, &join_data,
            password.as_deref(), locale_ctx.as_ref(), locale,
            user_doc.as_ref(), draft,
        )
    }).await;

    match result {
        Ok(Ok(doc)) => {
            state.hook_runner.fire_after_event(
                &def.hooks, &def.fields, HookEvent::AfterChange,
                slug.clone(), "create".to_string(), doc.fields.clone(),
            );
            state.hook_runner.publish_event(
                &state.event_bus, &def.hooks, def.live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Create,
                slug.clone(), doc.id.clone(), doc.fields.clone(),
            );

            // Auto-send verification email for auth collections with verify_email enabled
            if def.is_auth_collection() && def.auth.as_ref().is_some_and(|a| a.verify_email) {
                if let Some(user_email) = doc.fields.get("email").and_then(|v| v.as_str()) {
                    crate::service::send_verification_email(
                        state.pool.clone(),
                        state.config.email.clone(),
                        state.email_renderer.clone(),
                        state.config.server.clone(),
                        slug.clone(),
                        doc.id.clone(),
                        user_email.to_string(),
                    );
                }
            }

            redirect_response(&format!("/admin/collections/{}", slug))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let error_map = ve.to_field_map();
                let fields = build_field_contexts(&def.fields, &form_data_clone, &error_map, true, false);
                let data = serde_json::json!({
                    "page_title": format!("Create {}", def.singular_name()),
                    "collections": state.sidebar_collections(),
                    "globals": state.sidebar_globals(),
                    "collection": {
                        "slug": def.slug,
                        "display_name": def.display_name(),
                        "singular_name": def.singular_name(),
                    },
                    "fields": fields,
                    "editing": false,
                    "is_upload": def.is_upload_collection(),
                });
                html_with_toast(&state, "collections/edit", &data, &e.to_string())
            } else {
                tracing::error!("Create error: {}", e);
                redirect_response(&format!("/admin/collections/{}/create", slug))
            }
        }
        Err(e) => {
            tracing::error!("Create task error: {}", e);
            redirect_response(&format!("/admin/collections/{}/create", slug))
        }
    }
}

/// GET /admin/collections/{slug}/{id} — show edit form
pub async fn edit_form(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    Query(locale_params): Query<LocaleParams>,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(e) => return server_error(&state, &format!("Registry lock poisoned: {}", e)).into_response(),
        };
        match reg.get_collection(&slug) {
            Some(d) => d.clone(),
            None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
        }
    };

    // Check read access
    let access_result = match check_access_or_forbid(
        &state, def.access.read.as_deref(), &auth_user, Some(&id), None,
    ) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if matches!(access_result, AccessResult::Denied) {
        return forbidden(&state, "You don't have permission to view this item").into_response();
    }

    let (locale_ctx, locale_data) = build_locale_template_data(&state, locale_params.locale.as_deref());
    let locale_ctx_for_hydrate = locale_ctx.clone();

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let fields = def.fields.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let def_owned = def.clone();
    let access_constraints = if let AccessResult::Constrained(ref filters) = access_result {
        Some(filters.clone())
    } else {
        None
    };
    let read_result = tokio::task::spawn_blocking(move || {
        runner.fire_before_read(&hooks, &slug_owned, "find_by_id", HashMap::new())?;
        // If constrained, use find with id filter + constraints instead of find_by_id
        let doc = if let Some(constraints) = access_constraints {
            let mut filters = constraints;
            filters.push(FilterClause::Single(Filter {
                field: "id".to_string(),
                op: FilterOp::Equals(id_owned.clone()),
            }));
            let query = FindQuery { filters, ..Default::default() };
            let docs = ops::find_documents(&pool, &slug_owned, &def_owned, &query, locale_ctx.as_ref())?;
            docs.into_iter().next()
        } else {
            ops::find_document_by_id(&pool, &slug_owned, &def_owned, &id_owned, locale_ctx.as_ref())?
        };
        // Assemble sizes for upload collections
        let doc = doc.map(|mut d| {
            if let Some(ref upload_config) = def_owned.upload {
                if upload_config.enabled {
                    upload::assemble_sizes_object(&mut d, upload_config);
                }
            }
            d
        });
        let doc = doc.map(|d| runner.apply_after_read(&hooks, &fields, &slug_owned, "find_by_id", d));
        Ok::<_, anyhow::Error>(doc)
    }).await;

    let mut document = match read_result {
        Ok(Ok(Some(doc))) => doc,
        Ok(Ok(None)) => return not_found(&state, &format!("Document '{}' not found", id)).into_response(),
        Ok(Err(e)) => return server_error(&state, &format!("Query error: {}", e)).into_response(),
        Err(e) => return server_error(&state, &format!("Task error: {}", e)).into_response(),
    };

    // Hydrate join table data (has-many relationships and arrays)
    if let Ok(conn) = state.pool.get() {
        if let Err(e) = query::hydrate_document(&conn, &slug, &def, &mut document, None, locale_ctx_for_hydrate.as_ref()) {
            tracing::warn!("Failed to hydrate document {}: {}", id, e);
        }
    }

    // Strip field-level read-denied fields
    {
        let user_doc = get_user_doc(&auth_user);
        if let Ok(conn) = state.pool.get() {
            let denied = state.hook_runner.check_field_read_access(&def.fields, user_doc, &conn);
            strip_denied_fields(&mut document.fields, &denied);
        }
    }

    let values: HashMap<String, String> = document.fields.iter()
        .map(|(k, v)| {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            (k.clone(), s)
        })
        .collect();

    let non_default_locale = is_non_default_locale(&state, locale_params.locale.as_deref());
    let mut fields = build_field_contexts(&def.fields, &values, &HashMap::new(), true, non_default_locale);

    // Enrich relationship and array fields with extra data
    enrich_field_contexts(&mut fields, &def.fields, &document.fields, &state, true, non_default_locale);

    if def.is_auth_collection() {
        fields.push(serde_json::json!({
            "name": "password",
            "field_type": "password",
            "label": "Password",
            "required": false,
            "value": "",
            "description": "Leave blank to keep current password",
        }));
    }

    // Determine document title for breadcrumb
    let doc_title = def.title_field()
        .and_then(|f| document.get_str(f))
        .map(|s| s.to_string())
        .unwrap_or_else(|| document.id.clone());

    // Fetch document status and version history for versioned collections
    let has_drafts = def.has_drafts();
    let has_versions = def.has_versions();
    let doc_status = if has_drafts {
        document.fields.get("_status")
            .and_then(|v| v.as_str())
            .unwrap_or("published")
            .to_string()
    } else {
        String::new()
    };
    let (versions, total_versions): (Vec<serde_json::Value>, i64) = if has_versions {
        if let Ok(conn) = state.pool.get() {
            let total = query::count_versions(&conn, &slug, &document.id).unwrap_or(0);
            let vers = query::list_versions(&conn, &slug, &document.id, Some(3), None)
                .unwrap_or_default()
                .into_iter()
                .map(|v| serde_json::json!({
                    "id": v.id,
                    "version": v.version,
                    "status": v.status,
                    "latest": v.latest,
                    "created_at": v.created_at,
                }))
                .collect();
            (vers, total)
        } else {
            (vec![], 0)
        }
    } else {
        (vec![], 0)
    };

    let mut data = serde_json::json!({
        "page_title": format!("Edit {}", def.singular_name()),
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
        "collection": {
            "slug": def.slug,
            "display_name": def.display_name(),
            "singular_name": def.singular_name(),
        },
        "document": {
            "id": document.id,
            "created_at": document.created_at,
            "updated_at": document.updated_at,
            "status": doc_status,
        },
        "fields": fields,
        "editing": true,
        "has_drafts": has_drafts,
        "has_versions": has_versions,
        "versions": versions,
        "has_more_versions": total_versions > 3,
        "user": user_json(&claims),
        "breadcrumbs": [
            { "label": "Collections", "url": "/admin/collections" },
            { "label": def.display_name(), "url": format!("/admin/collections/{}", slug) },
            { "label": doc_title },
        ],
    });

    // Merge locale data into template context
    if let Some(obj) = locale_data.as_object() {
        for (k, v) in obj {
            data[k] = v.clone();
        }
    }

    // Add upload context for upload collections
    if def.is_upload_collection() {
        data["is_upload"] = serde_json::json!(true);
        if let Some(ref u) = def.upload {
            if !u.mime_types.is_empty() {
                data["upload_accept"] = serde_json::json!(u.mime_types.join(","));
            }
        }

        // Upload preview and file info from existing document
        let url = document.fields.get("url").and_then(|v| v.as_str());
        let mime_type = document.fields.get("mime_type").and_then(|v| v.as_str());
        let filename = document.fields.get("filename").and_then(|v| v.as_str());
        let filesize = document.fields.get("filesize").and_then(|v| v.as_f64()).map(|v| v as u64);
        let width = document.fields.get("width").and_then(|v| v.as_f64()).map(|v| v as u32);
        let height = document.fields.get("height").and_then(|v| v.as_f64()).map(|v| v as u32);

        // Show preview for images
        if let (Some(url), Some(mime)) = (url, mime_type) {
            if mime.starts_with("image/") {
                // Use admin_thumbnail size if available
                let preview_url = def.upload.as_ref()
                    .and_then(|u| u.admin_thumbnail.as_ref())
                    .and_then(|thumb_name| {
                        document.fields.get("sizes")
                            .and_then(|v| v.get(thumb_name))
                            .and_then(|v| v.get("url"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| url.to_string());
                data["upload_preview"] = serde_json::json!(preview_url);
            }
        }

        if let Some(fname) = filename {
            let mut info = serde_json::json!({
                "filename": fname,
            });
            if let Some(size) = filesize {
                info["filesize_display"] = serde_json::json!(upload::format_filesize(size));
            }
            if let (Some(w), Some(h)) = (width, height) {
                info["dimensions"] = serde_json::json!(format!("{}x{}", w, h));
            }
            data["upload_info"] = info;
        }
    }

    render_or_error(&state, "collections/edit", &data).into_response()
}

/// POST handler for update/delete (HTML forms use _method override).
pub async fn update_action_post(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(_) => return redirect_response("/admin/collections"),
        };
        reg.get_collection(&slug).cloned()
    };
    let def = match def {
        Some(d) => d,
        None => return redirect_response("/admin/collections"),
    };

    // Parse form data — multipart for upload collections, regular form for others
    let (mut form_data, file) = if def.is_upload_collection() {
        match parse_multipart_form(request, &state).await {
            Ok(result) => result,
            Err(e) => {
                tracing::error!("Multipart parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/{}", slug, id));
            }
        }
    } else {
        let Form(data) = match Form::<HashMap<String, String>>::from_request(request, &state).await {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Form parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/{}", slug, id));
            }
        };
        (data, None)
    };

    let method = form_data.remove("_method").unwrap_or_default();

    if method.eq_ignore_ascii_case("DELETE") {
        return delete_action_impl(&state, &slug, &id, &auth_user).await.into_response();
    }

    do_update(&state, &slug, &id, form_data, file, &auth_user).await
}

async fn do_update(state: &AdminState, slug: &str, id: &str, mut form_data: HashMap<String, String>, file: Option<UploadedFile>, auth_user: &Option<Extension<AuthUser>>) -> axum::response::Response {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(_) => return redirect_response("/admin/collections"),
        };
        reg.get_collection(slug).cloned()
    };
    let def = match def {
        Some(d) => d,
        None => return redirect_response("/admin/collections"),
    };

    // Extract action (publish/save_draft/unpublish) and locale
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let form_locale = form_data.remove("_locale");
    let locale_ctx = LocaleContext::from_locale_string(
        form_locale.as_deref(), &state.config.locale,
    );

    // Check update access
    match check_access_or_forbid(
        state, def.access.update.as_deref(), auth_user, Some(id), None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(state, "You don't have permission to update this item").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    // For upload collections, if a new file was uploaded, process it and delete old files
    let mut old_doc_fields: Option<HashMap<String, serde_json::Value>> = None;
    if let Some(f) = file {
        if let Some(ref upload_config) = def.upload {
            // Load old document to get old file paths for cleanup
            if let Ok(conn) = state.pool.get() {
                if let Ok(Some(old_doc)) = query::find_by_id(&conn, slug, &def, id, None) {
                    old_doc_fields = Some(old_doc.fields.clone());
                }
            }

            match upload::process_upload(
                &f, upload_config, &state.config_dir, slug,
                state.config.upload.max_file_size,
            ) {
                Ok(processed) => inject_upload_metadata(&mut form_data, &processed),
                Err(e) => {
                    tracing::error!("Upload processing error: {}", e);
                    let fields = build_field_contexts(&def.fields, &form_data, &HashMap::new(), true, false);
                    let data = serde_json::json!({
                        "page_title": format!("Edit {}", def.singular_name()),
                        "collections": state.sidebar_collections(),
                        "globals": state.sidebar_globals(),
                        "collection": {
                            "slug": def.slug,
                            "display_name": def.display_name(),
                            "singular_name": def.singular_name(),
                        },
                        "document": { "id": id },
                        "fields": fields,
                        "editing": true,
                        "is_upload": true,
                    });
                    return html_with_toast(state, "collections/edit", &data, &e.to_string());
                }
            }
        }
    }

    // Strip field-level update-denied fields
    {
        let user_doc = get_user_doc(auth_user);
        if let Ok(conn) = state.pool.get() {
            let denied = state.hook_runner.check_field_write_access(&def.fields, user_doc, "update", &conn);
            for name in &denied {
                form_data.remove(name);
            }
        }
    }

    // Extract password before it enters hooks/regular data flow
    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };

    // Extract join table data (arrays + has-many relationships) from form
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.to_string();
    let id_owned = id.to_string();
    let def_owned = def.clone();
    let form_data_clone = form_data.clone();
    let user_doc = get_user_doc(auth_user).cloned();
    let locale = locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        query::LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let action_owned = action.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Handle unpublish: set _status to 'draft' and create a version
        if action_owned == "unpublish" && def_owned.has_versions() {
            let mut conn = pool.get().map_err(|e| anyhow::anyhow!("DB connection: {}", e))?;
            let tx = conn.transaction().map_err(|e| anyhow::anyhow!("Start transaction: {}", e))?;
            query::set_document_status(&tx, &slug_owned, &id_owned, "draft")?;
            let doc = query::find_by_id(&tx, &slug_owned, &def_owned, &id_owned, None)?
                .ok_or_else(|| anyhow::anyhow!("Document not found"))?;
            let snapshot = query::build_snapshot(&tx, &slug_owned, &def_owned, &doc)?;
            query::create_version(&tx, &slug_owned, &id_owned, "draft", &snapshot)?;
            if let Some(ref vc) = def_owned.versions {
                if vc.max_versions > 0 {
                    query::prune_versions(&tx, &slug_owned, &id_owned, vc.max_versions)?;
                }
            }
            tx.commit().map_err(|e| anyhow::anyhow!("Commit: {}", e))?;
            Ok(doc)
        } else {
            crate::service::update_document(
                &pool, &runner, &slug_owned, &id_owned, &def_owned,
                form_data, &join_data,
                password.as_deref(), locale_ctx.as_ref(), locale,
                user_doc.as_ref(), draft,
            )
        }
    }).await;

    let locale_suffix = form_locale
        .as_ref()
        .filter(|_| state.config.locale.is_enabled())
        .map(|l| format!("?locale={}", l))
        .unwrap_or_default();
    match result {
        Ok(Ok(doc)) => {
            // If a new file was uploaded and old files exist, clean up old files
            if let Some(old_fields) = old_doc_fields {
                upload::delete_upload_files(&state.config_dir, &old_fields);
            }

            state.hook_runner.fire_after_event(
                &def.hooks, &def.fields, HookEvent::AfterChange,
                slug.to_string(), "update".to_string(), doc.fields.clone(),
            );
            state.hook_runner.publish_event(
                &state.event_bus, &def.hooks, def.live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Update,
                slug.to_string(), id.to_string(), doc.fields.clone(),
            );
            redirect_response(&format!("/admin/collections/{}/{}{}", slug, id, locale_suffix))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let error_map = ve.to_field_map();
                let fields = build_field_contexts(&def.fields, &form_data_clone, &error_map, true, false);
                let data = serde_json::json!({
                    "page_title": format!("Edit {}", def.singular_name()),
                    "collections": state.sidebar_collections(),
                    "globals": state.sidebar_globals(),
                    "collection": {
                        "slug": def.slug,
                        "display_name": def.display_name(),
                        "singular_name": def.singular_name(),
                    },
                    "document": {
                        "id": id,
                    },
                    "fields": fields,
                    "editing": true,
                    "is_upload": def.is_upload_collection(),
                });
                html_with_toast(state, "collections/edit", &data, &e.to_string())
            } else {
                tracing::error!("Update error: {}", e);
                redirect_response(&format!("/admin/collections/{}/{}", slug, id))
            }
        }
        Err(e) => {
            tracing::error!("Update task error: {}", e);
            redirect_response(&format!("/admin/collections/{}/{}", slug, id))
        }
    }
}

/// GET /admin/collections/{slug}/{id}/delete — delete confirmation page
pub async fn delete_confirm(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(e) => return server_error(&state, &format!("Registry lock poisoned: {}", e)).into_response(),
        };
        match reg.get_collection(&slug) {
            Some(d) => d.clone(),
            None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
        }
    };

    // Check delete access
    match check_access_or_forbid(
        &state, def.access.delete.as_deref(), &auth_user, Some(&id), None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to delete this item").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let document = match ops::find_document_by_id(&state.pool, &slug, &def, &id, None) {
        Ok(Some(doc)) => doc,
        Ok(None) => return not_found(&state, &format!("Document '{}' not found", id)).into_response(),
        Err(e) => return server_error(&state, &format!("Query error: {}", e)).into_response(),
    };

    let title_value = def.title_field()
        .and_then(|f| document.get_str(f))
        .map(|s| s.to_string());

    let data = serde_json::json!({
        "page_title": format!("Delete {}", def.singular_name()),
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
        "collection": {
            "slug": def.slug,
            "display_name": def.display_name(),
            "singular_name": def.singular_name(),
        },
        "document_id": id,
        "title_value": title_value,
        "user": user_json(&claims),
    });

    render_or_error(&state, "collections/delete", &data).into_response()
}

/// DELETE /admin/collections/{slug}/{id} — delete an item (no form body)
pub async fn delete_action_simple(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    delete_action_impl(&state, &slug, &id, &auth_user).await.into_response()
}

async fn delete_action_impl(state: &AdminState, slug: &str, id: &str, auth_user: &Option<Extension<AuthUser>>) -> axum::response::Response {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(_) => return axum::response::Redirect::to("/admin/collections").into_response(),
        };
        reg.get_collection(slug).cloned()
    };
    let def = match def {
        Some(d) => d,
        None => return axum::response::Redirect::to("/admin/collections").into_response(),
    };

    // Check delete access
    match check_access_or_forbid(
        state, def.access.delete.as_deref(), auth_user, Some(id), None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(state, "You don't have permission to delete this item").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    // For upload collections, load the document before deleting to get file paths
    let upload_doc_fields = if def.is_upload_collection() {
        state.pool.get().ok()
            .and_then(|conn| query::find_by_id(&conn, slug, &def, id, None).ok().flatten())
            .map(|doc| doc.fields.clone())
    } else {
        None
    };

    // Before hooks + delete in a single transaction
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let slug_owned = slug.to_string();
    let id_owned = id.to_string();
    let user_doc = get_user_doc(auth_user).cloned();
    let result = tokio::task::spawn_blocking(move || {
        crate::service::delete_document(
            &pool, &runner, &slug_owned, &id_owned, &hooks, user_doc.as_ref(),
        )
    }).await;

    match result {
        Ok(Ok(())) => {
            // Clean up upload files after successful delete
            if let Some(fields) = upload_doc_fields {
                upload::delete_upload_files(&state.config_dir, &fields);
            }

            state.hook_runner.fire_after_event(
                &def.hooks, &def.fields, HookEvent::AfterDelete,
                slug.to_string(), "delete".to_string(),
                [("id".to_string(), serde_json::Value::String(id.to_string()))].into(),
            );
            state.hook_runner.publish_event(
                &state.event_bus, &def.hooks, def.live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Delete,
                slug.to_string(), id.to_string(), std::collections::HashMap::new(),
            );
        }
        Ok(Err(e)) => {
            tracing::error!("Delete error: {}", e);
        }
        Err(e) => {
            tracing::error!("Delete task error: {}", e);
        }
    }

    axum::response::Redirect::to(&format!("/admin/collections/{}", slug)).into_response()
}

/// POST /admin/collections/{slug}/{id}/versions/{version_id}/restore — restore a version
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, id, version_id)): Path<(String, String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(_) => return redirect_response("/admin/collections"),
        };
        reg.get_collection(&slug).cloned()
    };
    let def = match def {
        Some(d) => d,
        None => return redirect_response("/admin/collections"),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/collections/{}/{}", slug, id));
    }

    // Check update access
    match check_access_or_forbid(
        &state, def.access.update.as_deref(), &auth_user, Some(&id), None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to update this item").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let pool = state.pool.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let def_owned = def.clone();
    let locale_config = state.config.locale.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut conn = pool.get().map_err(|e| anyhow::anyhow!("DB connection: {}", e))?;
        let tx = conn.transaction().map_err(|e| anyhow::anyhow!("Start transaction: {}", e))?;
        let version = query::find_version_by_id(&tx, &slug_owned, &version_id)?
            .ok_or_else(|| anyhow::anyhow!("Version not found"))?;
        let doc = query::restore_version(&tx, &slug_owned, &def_owned, &id_owned, &version.snapshot, "published", &locale_config)?;
        tx.commit().map_err(|e| anyhow::anyhow!("Commit: {}", e))?;
        Ok::<_, anyhow::Error>(doc)
    }).await;

    match result {
        Ok(Ok(_)) => redirect_response(&format!("/admin/collections/{}/{}", slug, id)),
        Ok(Err(e)) => {
            tracing::error!("Restore version error: {}", e);
            redirect_response(&format!("/admin/collections/{}/{}", slug, id))
        }
        Err(e) => {
            tracing::error!("Restore version task error: {}", e);
            redirect_response(&format!("/admin/collections/{}/{}", slug, id))
        }
    }
}

/// GET /admin/collections/{slug}/{id}/versions — dedicated version history page
pub async fn list_versions_page(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(e) => return server_error(&state, &format!("Registry lock poisoned: {}", e)).into_response(),
        };
        match reg.get_collection(&slug) {
            Some(d) => d.clone(),
            None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
        }
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/collections/{}/{}", slug, id)).into_response();
    }

    // Check read access
    match check_access_or_forbid(
        &state, def.access.read.as_deref(), &auth_user, Some(&id), None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to view this item").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    // Fetch the document for breadcrumb title
    let document = match ops::find_document_by_id(&state.pool, &slug, &def, &id, None) {
        Ok(Some(doc)) => doc,
        Ok(None) => return not_found(&state, &format!("Document '{}' not found", id)).into_response(),
        Err(e) => return server_error(&state, &format!("Query error: {}", e)).into_response(),
    };

    let doc_title = def.title_field()
        .and_then(|f| document.get_str(f))
        .map(|s| s.to_string())
        .unwrap_or_else(|| document.id.clone());

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(20).min(100);
    let offset = (page - 1) * per_page;

    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return server_error(&state, "Database error").into_response(),
    };

    let total = query::count_versions(&conn, &slug, &id).unwrap_or(0);
    let versions: Vec<serde_json::Value> = query::list_versions(&conn, &slug, &id, Some(per_page), Some(offset))
        .unwrap_or_default()
        .into_iter()
        .map(|v| serde_json::json!({
            "id": v.id,
            "version": v.version,
            "status": v.status,
            "latest": v.latest,
            "created_at": v.created_at,
        }))
        .collect();

    let total_pages = ((total as f64) / (per_page as f64)).ceil() as i64;

    let data = serde_json::json!({
        "page_title": format!("Version History — {}", doc_title),
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
        "collection": {
            "slug": def.slug,
            "display_name": def.display_name(),
            "singular_name": def.singular_name(),
        },
        "document": {
            "id": document.id,
        },
        "doc_title": doc_title,
        "versions": versions,
        "page": page,
        "per_page": per_page,
        "total": total,
        "total_pages": total_pages,
        "has_pagination": total_pages > 1,
        "has_prev": page > 1,
        "has_next": page < total_pages,
        "prev_url": format!("/admin/collections/{}/{}/versions?page={}", slug, id, page - 1),
        "next_url": format!("/admin/collections/{}/{}/versions?page={}", slug, id, page + 1),
        "user": user_json(&claims),
        "breadcrumbs": [
            { "label": "Collections", "url": "/admin/collections" },
            { "label": def.display_name(), "url": format!("/admin/collections/{}", slug) },
            { "label": doc_title, "url": format!("/admin/collections/{}/{}", slug, id) },
            { "label": "Version History" },
        ],
    });

    render_or_error(&state, "collections/versions", &data).into_response()
}
