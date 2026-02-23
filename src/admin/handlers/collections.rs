//! Collection CRUD handlers: list, create, edit, delete.

use axum::{
    extract::{Form, FromRequest, Multipart, Path, Query, State},
    response::{Html, IntoResponse, Redirect},
    Extension,
};
use serde::Deserialize;
use std::collections::HashMap;

use anyhow::Context as _;
use crate::admin::AdminState;
use crate::core::auth::{AuthUser, Claims};
use crate::core::field::FieldType;
use crate::core::upload::{self, UploadedFile, ProcessedUpload};
use crate::core::validate::ValidationError;
use crate::db::{ops, query};
use crate::db::query::{AccessResult, FindQuery, Filter, FilterOp, FilterClause};
use crate::hooks::lifecycle::{self, HookContext, HookEvent};

fn user_json(claims: &Option<Extension<Claims>>) -> Option<serde_json::Value> {
    claims.as_ref().map(|Extension(c)| serde_json::json!({
        "email": c.email,
        "id": c.sub,
        "collection": c.collection,
    }))
}

/// Extract the user document from AuthUser extension (for access checks).
fn get_user_doc(auth_user: &Option<Extension<AuthUser>>) -> Option<&crate::core::Document> {
    auth_user.as_ref().map(|Extension(au)| &au.user_doc)
}

/// Helper to check collection-level access. Returns AccessResult or renders a 403 page.
fn check_collection_access_or_forbid(
    state: &AdminState,
    access_ref: Option<&str>,
    auth_user: &Option<Extension<AuthUser>>,
    id: Option<&str>,
    data: Option<&HashMap<String, serde_json::Value>>,
) -> Result<AccessResult, axum::response::Response> {
    let user_doc = get_user_doc(auth_user);
    let conn = state.pool.get()
        .map_err(|_| forbidden(state, "Database error").into_response())?;
    state.hook_runner.check_access(access_ref, user_doc, id, data, &conn)
        .map_err(|e| {
            tracing::error!("Access check error: {}", e);
            forbidden(state, "Access check failed").into_response()
        })
}

/// Strip denied fields from a document's fields map.
fn strip_denied_fields(
    fields: &mut HashMap<String, serde_json::Value>,
    denied: &[String],
) {
    for name in denied {
        fields.remove(name);
    }
}

fn forbidden(state: &AdminState, message: &str) -> Html<String> {
    let data = serde_json::json!({
        "title": "Forbidden",
        "message": message,
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
    });
    match state.render("errors/403", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>403 Forbidden</h1><p>{}</p>", message)),
    }
}

/// Query parameters for paginated collection list views.
#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub search: Option<String>,
}

/// GET /admin/collections — list all registered collections
pub async fn list_collections(
    State(state): State<AdminState>,
    claims: Option<Extension<Claims>>,
) -> Html<String> {
    let mut collections = Vec::new();
    {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(e) => return server_error(&state, &format!("Registry lock poisoned: {}", e)),
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

    render_or_error(&state, "collections/list", &data)
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
    let access_result = match check_collection_access_or_forbid(
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
            filters.push(FilterClause::Or(or_filters));
        }
    }

    let find_query = FindQuery {
        filters: filters.clone(),
        order_by: def.admin.default_sort.clone(),
        limit: Some(per_page),
        offset: Some(offset),
    };

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let fields = def.fields.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let read_result = tokio::task::spawn_blocking(move || {
        runner.fire_before_read(&hooks, &slug_owned, "find", HashMap::new())?;
        let total = ops::count_documents(&pool, &slug_owned, &def_owned, &filters)?;
        let mut docs = ops::find_documents(&pool, &slug_owned, &def_owned, &find_query)?;
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

    // Strip field-level read-denied fields (used to filter title display)
    let _denied_fields = {
        let user_doc = get_user_doc(&auth_user);
        let conn = match state.pool.get() {
            Ok(c) => c,
            Err(_) => return server_error(&state, "Database error").into_response(),
        };
        state.hook_runner.check_field_read_access(&def.fields, user_doc, &conn)
    };

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
        let mut item = serde_json::json!({
            "id": doc.id,
            "title_value": title_value,
            "created_at": doc.created_at,
            "updated_at": doc.updated_at,
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
    match check_collection_access_or_forbid(
        &state, def.access.create.as_deref(), &auth_user, None, None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to create items in this collection").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let mut fields = build_field_contexts(&def.fields, &HashMap::new(), &HashMap::new());

    // Enrich relationship and array fields
    enrich_field_contexts(&mut fields, &def.fields, &HashMap::new(), &state);

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
        "user": user_json(&claims),
    });

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
    match check_collection_access_or_forbid(
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
                    let fields = build_field_contexts(&def.fields, &form_data, &HashMap::new());
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

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let is_auth = def.is_auth_collection();
    let form_data_clone = form_data.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut conn = pool.get().context("DB connection")?;
        let tx = conn.transaction().context("Start transaction")?;

        let hook_ctx = HookContext {
            collection: slug_owned.clone(),
            operation: "create".to_string(),
            data: form_data.iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
        };
        let final_ctx = runner.run_before_write(
            &hooks, &def_owned.fields, hook_ctx, &tx, &slug_owned, None,
        )?;
        let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx);
        let doc = query::create(&tx, &slug_owned, &def_owned, &final_data)?;

        // Save join table data (has-many relationships and arrays)
        if !join_data.is_empty() {
            query::save_join_table_data(&tx, &slug_owned, &def_owned, &doc.id, &join_data)?;
        }

        // Hash and store password for auth collections
        if is_auth {
            if let Some(ref pw) = password {
                if !pw.is_empty() {
                    query::update_password(&tx, &slug_owned, &doc.id, pw)?;
                }
            }
        }

        tx.commit().context("Commit transaction")?;
        Ok::<_, anyhow::Error>(doc)
    }).await;

    match result {
        Ok(Ok(doc)) => {
            state.hook_runner.fire_after_event(
                &def.hooks, &def.fields, HookEvent::AfterChange,
                slug.clone(), "create".to_string(), doc.fields.clone(),
            );
            redirect_response(&format!("/admin/collections/{}", slug))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let error_map = ve.to_field_map();
                let fields = build_field_contexts(&def.fields, &form_data_clone, &error_map);
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
    let access_result = match check_collection_access_or_forbid(
        &state, def.access.read.as_deref(), &auth_user, Some(&id), None,
    ) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if matches!(access_result, AccessResult::Denied) {
        return forbidden(&state, "You don't have permission to view this item").into_response();
    }

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
            let docs = ops::find_documents(&pool, &slug_owned, &def_owned, &query)?;
            docs.into_iter().next()
        } else {
            ops::find_document_by_id(&pool, &slug_owned, &def_owned, &id_owned)?
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
        if let Err(e) = query::hydrate_document(&conn, &slug, &def, &mut document) {
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

    let mut fields = build_field_contexts(&def.fields, &values, &HashMap::new());

    // Enrich relationship and array fields with extra data
    enrich_field_contexts(&mut fields, &def.fields, &document.fields, &state);

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
        },
        "fields": fields,
        "editing": true,
        "user": user_json(&claims),
    });

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

    // Check update access
    match check_collection_access_or_forbid(
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
                if let Ok(Some(old_doc)) = query::find_by_id(&conn, slug, &def, id) {
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
                    let fields = build_field_contexts(&def.fields, &form_data, &HashMap::new());
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
    let hooks = def.hooks.clone();
    let slug_owned = slug.to_string();
    let id_owned = id.to_string();
    let is_auth = def.is_auth_collection();
    let def_owned = def.clone();
    let form_data_clone = form_data.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut conn = pool.get().context("DB connection")?;
        let tx = conn.transaction().context("Start transaction")?;

        let hook_ctx = HookContext {
            collection: slug_owned.clone(),
            operation: "update".to_string(),
            data: form_data.iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
        };
        let final_ctx = runner.run_before_write(
            &hooks, &def_owned.fields, hook_ctx, &tx, &slug_owned, Some(&id_owned),
        )?;
        let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx);
        let doc = query::update(&tx, &slug_owned, &def_owned, &id_owned, &final_data)?;

        // Save join table data (has-many relationships and arrays)
        query::save_join_table_data(&tx, &slug_owned, &def_owned, &doc.id, &join_data)?;

        // Update password if provided (auth collections only)
        if is_auth {
            if let Some(ref pw) = password {
                if !pw.is_empty() {
                    query::update_password(&tx, &slug_owned, &doc.id, pw)?;
                }
            }
        }

        tx.commit().context("Commit transaction")?;
        Ok::<_, anyhow::Error>(doc)
    }).await;

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
            redirect_response(&format!("/admin/collections/{}/{}", slug, id))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let error_map = ve.to_field_map();
                let fields = build_field_contexts(&def.fields, &form_data_clone, &error_map);
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
    match check_collection_access_or_forbid(
        &state, def.access.delete.as_deref(), &auth_user, Some(&id), None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to delete this item").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let document = match ops::find_document_by_id(&state.pool, &slug, &def, &id) {
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
            Err(_) => return Redirect::to("/admin/collections").into_response(),
        };
        reg.get_collection(slug).cloned()
    };
    let def = match def {
        Some(d) => d,
        None => return Redirect::to("/admin/collections").into_response(),
    };

    // Check delete access
    match check_collection_access_or_forbid(
        state, def.access.delete.as_deref(), auth_user, Some(id), None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(state, "You don't have permission to delete this item").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    // For upload collections, load the document before deleting to get file paths
    let upload_doc_fields = if def.is_upload_collection() {
        state.pool.get().ok()
            .and_then(|conn| query::find_by_id(&conn, slug, &def, id).ok().flatten())
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
    let result = tokio::task::spawn_blocking(move || {
        let mut conn = pool.get().context("DB connection")?;
        let tx = conn.transaction().context("Start transaction")?;

        let hook_ctx = HookContext {
            collection: slug_owned.clone(),
            operation: "delete".to_string(),
            data: [("id".to_string(), serde_json::Value::String(id_owned.clone()))].into(),
        };
        runner.run_hooks_with_conn(
            &hooks, HookEvent::BeforeDelete, hook_ctx, &tx,
        )?;
        query::delete(&tx, &slug_owned, &id_owned)?;
        tx.commit().context("Commit transaction")?;
        Ok::<_, anyhow::Error>(())
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
        }
        Ok(Err(e)) => {
            tracing::error!("Delete error: {}", e);
        }
        Err(e) => {
            tracing::error!("Delete task error: {}", e);
        }
    }

    Redirect::to(&format!("/admin/collections/{}", slug)).into_response()
}

// --- Helpers ---

/// Build field context objects for template rendering.
fn build_field_contexts(
    fields: &[crate::core::field::FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
) -> Vec<serde_json::Value> {
    fields.iter().filter(|field| !field.admin.hidden).map(|field| {
        let value = values.get(&field.name).cloned().unwrap_or_default();
        let label = field.name.split('_')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().chain(c).collect(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        let mut ctx = serde_json::json!({
            "name": field.name,
            "field_type": field.field_type.as_str(),
            "label": label,
            "required": field.required,
            "value": value,
            "placeholder": field.admin.placeholder,
            "description": field.admin.description,
            "readonly": field.admin.readonly,
        });

        if let Some(err) = errors.get(&field.name) {
            ctx["error"] = serde_json::json!(err);
        }

        match &field.field_type {
            FieldType::Select => {
                let options: Vec<_> = field.options.iter().map(|opt| {
                    serde_json::json!({
                        "label": opt.label,
                        "value": opt.value,
                        "selected": opt.value == value,
                    })
                }).collect();
                ctx["options"] = serde_json::json!(options);
            }
            FieldType::Checkbox => {
                let checked = matches!(value.as_str(), "1" | "true" | "on" | "yes");
                ctx["checked"] = serde_json::json!(checked);
            }
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    ctx["relationship_collection"] = serde_json::json!(rc.collection);
                    ctx["has_many"] = serde_json::json!(rc.has_many);
                }
            }
            FieldType::Array => {
                // Provide sub-field definitions for template rendering
                let sub_fields: Vec<_> = field.fields.iter().map(|sf| {
                    serde_json::json!({
                        "name": sf.name,
                        "field_type": sf.field_type.as_str(),
                        "label": sf.name.split('_')
                            .map(|w| {
                                let mut c = w.chars();
                                match c.next() {
                                    None => String::new(),
                                    Some(f) => f.to_uppercase().chain(c).collect(),
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" "),
                        "required": sf.required,
                    })
                }).collect();
                ctx["sub_fields"] = serde_json::json!(sub_fields);
                ctx["row_count"] = serde_json::json!(0);
            }
            _ => {}
        }

        ctx
    }).collect()
}

/// Enrich field contexts with data that requires DB access:
/// - Relationship fields: fetch available options from related collection
/// - Array fields: populate existing rows from hydrated document data
fn enrich_field_contexts(
    fields: &mut [serde_json::Value],
    field_defs: &[crate::core::field::FieldDefinition],
    doc_fields: &HashMap<String, serde_json::Value>,
    state: &AdminState,
) {
    let reg = match state.registry.read() {
        Ok(r) => r,
        Err(_) => return,
    };
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };

    for (ctx, field_def) in fields.iter_mut().zip(field_defs.iter().filter(|f| !f.admin.hidden)) {
        match field_def.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field_def.relationship {
                    // Fetch documents from related collection for options
                    if let Some(related_def) = reg.get_collection(&rc.collection) {
                        let title_field = related_def.title_field().map(|s| s.to_string());
                        let find_query = query::FindQuery::default();
                        if let Ok(docs) = query::find(&conn, &rc.collection, related_def, &find_query) {
                            if rc.has_many {
                                // Get selected IDs from hydrated document
                                let selected_ids: std::collections::HashSet<String> = match doc_fields.get(&field_def.name) {
                                    Some(serde_json::Value::Array(arr)) => {
                                        arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                                    }
                                    _ => std::collections::HashSet::new(),
                                };
                                let options: Vec<_> = docs.iter().map(|doc| {
                                    let label = title_field.as_ref()
                                        .and_then(|f| doc.get_str(f))
                                        .unwrap_or(&doc.id);
                                    serde_json::json!({
                                        "value": doc.id,
                                        "label": label,
                                        "selected": selected_ids.contains(&doc.id),
                                    })
                                }).collect();
                                ctx["relationship_options"] = serde_json::json!(options);
                            } else {
                                // Has-one: current value from context
                                let current_value = ctx.get("value")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let options: Vec<_> = docs.iter().map(|doc| {
                                    let label = title_field.as_ref()
                                        .and_then(|f| doc.get_str(f))
                                        .unwrap_or(&doc.id);
                                    serde_json::json!({
                                        "value": doc.id,
                                        "label": label,
                                        "selected": doc.id == current_value,
                                    })
                                }).collect();
                                ctx["relationship_options"] = serde_json::json!(options);
                            }
                        }
                    }
                }
            }
            FieldType::Array => {
                // Populate rows from hydrated document data
                let rows: Vec<serde_json::Value> = match doc_fields.get(&field_def.name) {
                    Some(serde_json::Value::Array(arr)) => {
                        arr.iter().enumerate().map(|(idx, row)| {
                            let row_obj = row.as_object();
                            let sub_values: Vec<_> = field_def.fields.iter().map(|sf| {
                                let val = row_obj
                                    .and_then(|m| m.get(&sf.name))
                                    .map(|v| match v {
                                        serde_json::Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .unwrap_or_default();
                                serde_json::json!({
                                    "name": sf.name,
                                    "field_type": sf.field_type.as_str(),
                                    "value": val,
                                    "field_name_indexed": format!("{}[{}][{}]", field_def.name, idx, sf.name),
                                })
                            }).collect();
                            serde_json::json!({
                                "index": idx,
                                "sub_fields": sub_values,
                            })
                        }).collect()
                    }
                    _ => Vec::new(),
                };
                ctx["row_count"] = serde_json::json!(rows.len());
                ctx["rows"] = serde_json::json!(rows);
            }
            _ => {}
        }
    }
}

/// Extract join table data from form submission for has-many relationships and array fields.
/// Returns a map suitable for `query::save_join_table_data`.
fn extract_join_data_from_form(
    form: &HashMap<String, String>,
    field_defs: &[crate::core::field::FieldDefinition],
) -> HashMap<String, serde_json::Value> {
    let mut join_data = HashMap::new();

    for field in field_defs {
        match field.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        // Has-many: comma-separated IDs in form value
                        if let Some(val) = form.get(&field.name) {
                            join_data.insert(field.name.clone(), serde_json::Value::String(val.clone()));
                        } else {
                            // Empty selection — clear all
                            join_data.insert(field.name.clone(), serde_json::Value::String(String::new()));
                        }
                    }
                }
            }
            FieldType::Array => {
                let rows = parse_array_form_data(form, &field.name);
                let json_rows: Vec<serde_json::Value> = rows.into_iter()
                    .map(|row| {
                        let obj: serde_json::Map<String, serde_json::Value> = row.into_iter()
                            .map(|(k, v)| (k, serde_json::Value::String(v)))
                            .collect();
                        serde_json::Value::Object(obj)
                    })
                    .collect();
                join_data.insert(field.name.clone(), serde_json::Value::Array(json_rows));
            }
            _ => {}
        }
    }

    join_data
}

/// Parse array sub-field data from flat form keys.
/// Converts keys like `slides[0][title]`, `slides[1][caption]` into
/// a Vec of row hashmaps.
fn parse_array_form_data(form: &HashMap<String, String>, field_name: &str) -> Vec<HashMap<String, String>> {
    let prefix = format!("{}[", field_name);
    let mut rows: std::collections::BTreeMap<usize, HashMap<String, String>> = std::collections::BTreeMap::new();

    for (key, value) in form {
        if let Some(rest) = key.strip_prefix(&prefix) {
            // rest looks like "0][title]"
            if let Some(bracket_pos) = rest.find(']') {
                if let Ok(idx) = rest[..bracket_pos].parse::<usize>() {
                    // After "]" we expect "[fieldname]"
                    let after = &rest[bracket_pos + 1..];
                    if let Some(field_key) = after.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                        rows.entry(idx).or_default().insert(field_key.to_string(), value.clone());
                    }
                }
            }
        }
    }

    rows.into_values().collect()
}

/// Parse a multipart form request, extracting form fields and an optional file upload.
async fn parse_multipart_form(
    request: axum::extract::Request,
    state: &AdminState,
) -> Result<(HashMap<String, String>, Option<UploadedFile>), anyhow::Error> {
    let mut multipart = Multipart::from_request(request, state).await
        .map_err(|e| anyhow::anyhow!("Failed to parse multipart: {}", e))?;

    let mut form_data = HashMap::new();
    let mut file: Option<UploadedFile> = None;

    while let Some(field) = multipart.next_field().await
        .map_err(|e| anyhow::anyhow!("Failed to read multipart field: {}", e))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "_file" && field.file_name().is_some() {
            let filename = field.file_name().unwrap_or("").to_string();
            let content_type = field.content_type()
                .unwrap_or("application/octet-stream").to_string();
            let data = field.bytes().await
                .map_err(|e| anyhow::anyhow!("Failed to read file data: {}", e))?;
            if !data.is_empty() {
                file = Some(UploadedFile {
                    filename,
                    content_type,
                    data: data.to_vec(),
                });
            }
        } else {
            let text = field.text().await.unwrap_or_default();
            form_data.insert(name, text);
        }
    }

    Ok((form_data, file))
}

/// Inject upload metadata fields into form data from a processed upload.
/// Writes per-size typed fields ({name}_url, {name}_width, {name}_height, {name}_webp_url, etc.)
fn inject_upload_metadata(form_data: &mut HashMap<String, String>, processed: &ProcessedUpload) {
    form_data.insert("filename".into(), processed.filename.clone());
    form_data.insert("mime_type".into(), processed.mime_type.clone());
    form_data.insert("filesize".into(), processed.filesize.to_string());
    if let Some(w) = processed.width {
        form_data.insert("width".into(), w.to_string());
    }
    if let Some(h) = processed.height {
        form_data.insert("height".into(), h.to_string());
    }
    form_data.insert("url".into(), processed.url.clone());

    // Per-size typed fields
    for (name, size) in &processed.sizes {
        form_data.insert(format!("{}_url", name), size.url.clone());
        form_data.insert(format!("{}_width", name), size.width.to_string());
        form_data.insert(format!("{}_height", name), size.height.to_string());
        for (fmt, result) in &size.formats {
            form_data.insert(format!("{}_{}_url", name, fmt), result.url.clone());
        }
    }
}

fn redirect_response(url: &str) -> axum::response::Response {
    Redirect::to(url).into_response()
}

fn html_with_toast(state: &AdminState, template: &str, data: &serde_json::Value, toast: &str) -> axum::response::Response {
    match state.render(template, data) {
        Ok(html) => {
            let mut resp = Html(html).into_response();
            if let Ok(val) = toast.parse() {
                resp.headers_mut().insert("X-Crap-Toast", val);
            }
            resp
        }
        Err(e) => Html(format!("<h1>Template Error</h1><pre>{}</pre>", e)).into_response(),
    }
}

fn render_or_error(state: &AdminState, template: &str, data: &serde_json::Value) -> Html<String> {
    match state.render(template, data) {
        Ok(html) => Html(html),
        Err(e) => Html(format!("<h1>Template Error</h1><pre>{}</pre>", e)),
    }
}

fn not_found(state: &AdminState, message: &str) -> Html<String> {
    let data = serde_json::json!({
        "title": "Not Found",
        "message": message,
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
    });
    match state.render("errors/404", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>404</h1><p>{}</p>", message)),
    }
}

fn server_error(state: &AdminState, message: &str) -> Html<String> {
    let data = serde_json::json!({
        "title": "Server Error",
        "message": message,
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
    });
    match state.render("errors/500", &data) {
        Ok(html) => Html(html),
        Err(_) => Html(format!("<h1>500</h1><p>{}</p>", message)),
    }
}
