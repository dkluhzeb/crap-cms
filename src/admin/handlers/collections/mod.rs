//! Collection CRUD handlers: list, create, edit, delete.

pub(crate) mod forms;

use axum::{
    extract::{Form, FromRequest, Path, Query, State},
    response::IntoResponse,
    Extension,
};
use anyhow::Context as _;
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType, Breadcrumb};
use crate::core::auth::{AuthUser, Claims};
use crate::core::field::FieldType;
use crate::core::upload::{self, UploadedFile};
use crate::core::validate::ValidationError;
use crate::db::{ops, query};
use crate::db::query::{AccessResult, FindQuery, FilterOp, FilterClause, LocaleContext};

use super::shared::{
    PaginationParams,
    get_user_doc, get_event_user, strip_denied_fields,
    check_access_or_forbid, extract_editor_locale, build_locale_template_data,
    is_non_default_locale, auto_label_from_name, url_decode,
    parse_where_params, validate_sort, build_list_url, is_column_eligible,
    extract_where_params,
    build_field_contexts, enrich_field_contexts,
    apply_display_conditions, split_sidebar_fields,
    version_to_json, fetch_version_sidebar_data,
    forbidden, redirect_response, htmx_redirect, html_with_toast,
    render_or_error, not_found, server_error,
};

use crate::core::upload::inject_upload_metadata;
use forms::{extract_join_data_from_form, parse_multipart_form, transform_select_has_many};

/// GET /admin/collections — list all registered collections
pub async fn list_collections(
    State(state): State<AdminState>,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
) -> impl IntoResponse {
    let mut collections = Vec::new();
    for (slug, def) in &state.registry.collections {
        collections.push(serde_json::json!({
            "slug": slug,
            "display_name": def.display_name(),
            "field_count": def.fields.len(),
        }));
    }
    collections.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionList, "Collections")
        .set("collections", serde_json::json!(collections))
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/list", &data).into_response()
}

// ── List view helpers ─────────────────────────────────────────────────────

/// Get the display label for a field (admin label or auto-generated from name).
fn field_label(field: &crate::core::field::FieldDefinition) -> String {
    if let Some(ref label) = field.admin.label {
        label.resolve_default().to_string()
    } else {
        auto_label_from_name(&field.name)
    }
}

/// Resolve which columns to display in the list table.
///
/// Title column is always implicit (not in this vec). Returns vec of column metadata
/// with sort URLs and active sort state.
fn resolve_columns(
    def: &crate::core::CollectionDefinition,
    user_cols: Option<&[String]>,
    sort: Option<&str>,
    base_url: &str,
    raw_where: &str,
    search: Option<&str>,
) -> Vec<serde_json::Value> {
    let mut keys: Vec<String> = if let Some(cols) = user_cols {
        // Validate user columns against eligible fields + system cols
        cols.iter()
            .filter(|k| {
                k.as_str() == "created_at" || k.as_str() == "updated_at" || k.as_str() == "_status"
                    || def.fields.iter().any(|f| f.name == **k && is_column_eligible(&f.field_type))
            })
            .cloned()
            .collect()
    } else {
        // Defaults: status (if has_drafts) + created_at
        let mut defaults = Vec::new();
        if def.has_drafts() {
            defaults.push("_status".to_string());
        }
        defaults.push("created_at".to_string());
        defaults
    };
    // Remove title field from columns if it snuck in (it's always first, separate)
    if let Some(title) = def.title_field() {
        keys.retain(|k| k != title);
    }

    let sort_field = sort.map(|s| s.strip_prefix('-').unwrap_or(s));
    let sort_desc = sort.map(|s| s.starts_with('-')).unwrap_or(false);

    keys.iter().map(|key| {
        let (label, sortable) = match key.as_str() {
            "created_at" => ("Created".to_string(), true),
            "updated_at" => ("Updated".to_string(), true),
            "_status" => ("Status".to_string(), true),
            _ => {
                if let Some(f) = def.fields.iter().find(|f| f.name == *key) {
                    (field_label(f), true)
                } else {
                    (auto_label_from_name(key), false)
                }
            }
        };

        let is_sorted = sort_field == Some(key.as_str());
        let next_sort = if is_sorted && !sort_desc {
            format!("-{}", key)
        } else {
            key.clone()
        };
        let sort_url = build_list_url(base_url, 1, None, search, Some(&next_sort), raw_where);

        serde_json::json!({
            "key": key,
            "label": label,
            "sortable": sortable,
            "sort_url": sort_url,
            "is_sorted_asc": is_sorted && !sort_desc,
            "is_sorted_desc": is_sorted && sort_desc,
        })
    }).collect()
}

/// Pre-compute cell values for a document row, parallel to the columns array.
fn compute_cells(
    doc: &crate::core::Document,
    columns: &[serde_json::Value],
    def: &crate::core::CollectionDefinition,
) -> Vec<serde_json::Value> {
    columns.iter().map(|col| {
        let key = col["key"].as_str().unwrap_or("");
        match key {
            "_status" => {
                let status = doc.fields.get("_status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("published");
                serde_json::json!({ "value": status, "is_badge": true })
            }
            "created_at" => {
                serde_json::json!({ "value": doc.created_at, "is_date": true })
            }
            "updated_at" => {
                serde_json::json!({ "value": doc.updated_at, "is_date": true })
            }
            _ => {
                let field_def = def.fields.iter().find(|f| f.name == key);
                let raw = doc.fields.get(key).cloned().unwrap_or(serde_json::Value::Null);

                if let Some(f) = field_def {
                    match f.field_type {
                        FieldType::Checkbox => {
                            let checked = match &raw {
                                serde_json::Value::Bool(b) => *b,
                                serde_json::Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
                                _ => false,
                            };
                            serde_json::json!({ "value": checked, "is_bool": true })
                        }
                        FieldType::Date => {
                            let val = raw.as_str().unwrap_or("");
                            serde_json::json!({ "value": val, "is_date": true })
                        }
                        FieldType::Select | FieldType::Radio => {
                            // Show option label instead of raw value
                            let raw_val = raw.as_str().unwrap_or("");
                            let label = f.options.iter()
                                .find(|o| o.value == raw_val)
                                .map(|o| o.label.resolve_default().to_string())
                                .unwrap_or_else(|| raw_val.to_string());
                            serde_json::json!({ "value": label })
                        }
                        FieldType::Textarea => {
                            let text = raw.as_str().unwrap_or("");
                            let truncated = if text.len() > 80 {
                                format!("{}…", &text[..80])
                            } else {
                                text.to_string()
                            };
                            serde_json::json!({ "value": truncated })
                        }
                        _ => {
                            let val = match &raw {
                                serde_json::Value::String(s) => s.clone(),
                                serde_json::Value::Number(n) => n.to_string(),
                                serde_json::Value::Bool(b) => b.to_string(),
                                serde_json::Value::Null => String::new(),
                                other => other.to_string(),
                            };
                            serde_json::json!({ "value": val })
                        }
                    }
                } else {
                    let val = match &raw {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    serde_json::json!({ "value": val })
                }
            }
        }
    }).collect()
}

/// Build the list of all eligible columns for the column picker UI.
fn build_column_options(
    def: &crate::core::CollectionDefinition,
    selected_keys: &[String],
) -> Vec<serde_json::Value> {
    let mut options = Vec::new();

    // System columns
    if def.has_drafts() {
        options.push(serde_json::json!({
            "key": "_status",
            "label": "Status",
            "selected": selected_keys.contains(&"_status".to_string()),
        }));
    }
    options.push(serde_json::json!({
        "key": "created_at",
        "label": "Created",
        "selected": selected_keys.contains(&"created_at".to_string()),
    }));
    options.push(serde_json::json!({
        "key": "updated_at",
        "label": "Updated",
        "selected": selected_keys.contains(&"updated_at".to_string()),
    }));

    // Field columns
    let title_field = def.title_field();
    for f in &def.fields {
        if Some(f.name.as_str()) == title_field {
            continue; // Title field is always shown, not toggleable
        }
        if is_column_eligible(&f.field_type) {
            options.push(serde_json::json!({
                "key": f.name,
                "label": field_label(f),
                "selected": selected_keys.contains(&f.name),
            }));
        }
    }

    options
}

/// Build filter field metadata for the filter builder UI.
fn build_filter_fields(def: &crate::core::CollectionDefinition) -> Vec<serde_json::Value> {
    let mut fields = Vec::new();

    // System columns
    if def.has_drafts() {
        fields.push(serde_json::json!({
            "key": "_status",
            "label": "Status",
            "field_type": "select",
            "options": [
                { "label": "Published", "value": "published" },
                { "label": "Draft", "value": "draft" },
            ],
        }));
    }
    fields.push(serde_json::json!({
        "key": "created_at",
        "label": "Created",
        "field_type": "date",
    }));
    fields.push(serde_json::json!({
        "key": "updated_at",
        "label": "Updated",
        "field_type": "date",
    }));

    // Collection fields
    for f in &def.fields {
        if !is_column_eligible(&f.field_type) {
            continue;
        }
        let ft = format!("{:?}", f.field_type).to_lowercase();
        let mut field_info = serde_json::json!({
            "key": f.name,
            "label": field_label(f),
            "field_type": ft,
        });
        if !f.options.is_empty() {
            let opts: Vec<serde_json::Value> = f.options.iter()
                .map(|o| serde_json::json!({
                    "label": o.label.resolve_default(),
                    "value": o.value,
                }))
                .collect();
            field_info["options"] = serde_json::json!(opts);
        }
        fields.push(field_info);
    }

    fields
}

/// Build active filter pills from parsed filter clauses.
fn build_filter_pills(
    parsed: &[FilterClause],
    def: &crate::core::CollectionDefinition,
    raw_query: &str,
) -> Vec<serde_json::Value> {
    parsed.iter().filter_map(|clause| {
        let FilterClause::Single(filter) = clause else { return None };
        let field_label_str = match filter.field.as_str() {
            "created_at" => "Created".to_string(),
            "updated_at" => "Updated".to_string(),
            "_status" => "Status".to_string(),
            name => def.fields.iter()
                .find(|f| f.name == name)
                .map(|f| field_label(f))
                .unwrap_or_else(|| auto_label_from_name(name)),
        };

        let (op_label, value) = match &filter.op {
            FilterOp::Equals(v) => ("is", v.clone()),
            FilterOp::NotEquals(v) => ("is not", v.clone()),
            FilterOp::Contains(v) => ("contains", v.clone()),
            FilterOp::Like(v) => ("like", v.clone()),
            FilterOp::GreaterThan(v) => (">", v.clone()),
            FilterOp::LessThan(v) => ("<", v.clone()),
            FilterOp::GreaterThanOrEqual(v) => (">=", v.clone()),
            FilterOp::LessThanOrEqual(v) => ("<=", v.clone()),
            FilterOp::Exists => ("exists", String::new()),
            FilterOp::NotExists => ("not exists", String::new()),
            _ => return None,
        };

        // Build remove URL: reconstruct query string without this filter
        let filter_key = format!("where[{}][{}]", filter.field, op_to_param_name(&filter.op));
        let remove_query: Vec<&str> = raw_query.split('&')
            .filter(|p| {
                let decoded = url_decode(p.split('=').next().unwrap_or(""));
                decoded != filter_key
            })
            .collect();
        let remove_url = if remove_query.is_empty() {
            String::new()
        } else {
            format!("?{}", remove_query.join("&"))
        };

        Some(serde_json::json!({
            "field_label": field_label_str,
            "op": op_label,
            "value": value,
            "remove_url": remove_url,
        }))
    }).collect()
}

/// Map a FilterOp to its URL parameter name.
fn op_to_param_name(op: &FilterOp) -> &'static str {
    match op {
        FilterOp::Equals(_) => "equals",
        FilterOp::NotEquals(_) => "not_equals",
        FilterOp::Contains(_) => "contains",
        FilterOp::Like(_) => "like",
        FilterOp::GreaterThan(_) => "gt",
        FilterOp::LessThan(_) => "lt",
        FilterOp::GreaterThanOrEqual(_) => "gte",
        FilterOp::LessThanOrEqual(_) => "lte",
        FilterOp::Exists => "exists",
        FilterOp::NotExists => "not_exists",
        _ => "equals",
    }
}

/// GET /admin/collections/{slug} — list items in a collection
pub async fn list_items(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Query(params): Query<PaginationParams>,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
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

    let raw_query = uri.query().unwrap_or("");
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page
        .unwrap_or(state.config.pagination.default_limit)
        .min(state.config.pagination.max_limit);
    let offset = (page - 1) * per_page;
    let search = params.search.filter(|s| !s.trim().is_empty());

    // Parse sort and where params
    let sort = params.sort.as_deref().and_then(|s| validate_sort(s, &def));
    let url_filters = parse_where_params(raw_query, &def);

    // Build search filters (OR across searchable fields)
    let mut filters: Vec<FilterClause> = Vec::new();

    // Merge access constraint filters
    if let AccessResult::Constrained(ref constraint_filters) = access_result {
        filters.extend(constraint_filters.clone());
    }

    // Merge URL filters
    filters.extend(url_filters.clone());

    // Determine sort order: URL param > default_sort
    let order_by = sort.clone().or_else(|| def.admin.default_sort.clone());

    let find_query = FindQuery {
        filters: filters.clone(),
        order_by,
        limit: Some(per_page),
        offset: Some(offset),
        select: None,
        after_cursor: None,
        before_cursor: None,
        search: search.clone(),
    };

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let locale_ctx = LocaleContext::from_locale_string(editor_locale.as_deref(), &state.config.locale);

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let fields = def.fields.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let read_result = tokio::task::spawn_blocking(move || {
        runner.fire_before_read(&hooks, &slug_owned, "find", HashMap::new())?;
        let conn = pool.get().context("Failed to get DB connection")?;
        let total = query::count_with_search(&conn, &slug_owned, &def_owned, &filters, locale_ctx.as_ref(), find_query.search.as_deref())?;
        let mut docs = query::find(&conn, &slug_owned, &def_owned, &find_query, locale_ctx.as_ref())?;
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
    let denied_fields = if def.fields.iter().any(|f| f.access.read.is_some()) {
        let user_doc = get_user_doc(&auth_user);
        let mut conn = match state.pool.get() {
            Ok(c) => c,
            Err(_) => return server_error(&state, "Database error").into_response(),
        };
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(_) => return server_error(&state, "Database error").into_response(),
        };
        let denied = state.hook_runner.check_field_read_access(&def.fields, user_doc, &tx);
        let _ = tx.commit();
        denied
    } else {
        Vec::new()
    };
    let documents: Vec<_> = documents.into_iter().map(|mut doc| {
        for field_name in &denied_fields {
            doc.fields.remove(field_name);
        }
        doc
    }).collect();

    // Load user column preferences
    let user_columns: Option<Vec<String>> = auth_user.as_ref().and_then(|Extension(au)| {
        let conn = state.pool.get().ok()?;
        let settings_json = query::auth::get_user_settings(&conn, &au.claims.sub).ok()??;
        let settings: serde_json::Value = serde_json::from_str(&settings_json).ok()?;
        let cols = settings.get(&slug)?.get("columns")?.as_array()?;
        Some(cols.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
    });

    let base_url = format!("/admin/collections/{}", slug);
    let where_params = extract_where_params(raw_query);

    // Resolve columns and build column keys for cell computation
    let table_columns = resolve_columns(
        &def,
        user_columns.as_deref(),
        sort.as_deref(),
        &base_url,
        &where_params,
        search.as_deref(),
    );
    let column_keys: Vec<String> = table_columns.iter()
        .filter_map(|c| c["key"].as_str().map(|s| s.to_string()))
        .collect();

    let column_options = build_column_options(&def, &column_keys);
    let filter_fields = build_filter_fields(&def);
    let filter_pills = build_filter_pills(&url_filters, &def, raw_query);

    // Build title column sort URL
    let title_field = def.title_field().map(|s| s.to_string());
    let title_sort_url = if let Some(ref tf) = title_field {
        let sort_field_name = sort.as_deref().map(|s| s.strip_prefix('-').unwrap_or(s));
        let sort_desc = sort.as_deref().map(|s| s.starts_with('-')).unwrap_or(false);
        let next = if sort_field_name == Some(tf.as_str()) && !sort_desc {
            format!("-{}", tf)
        } else {
            tf.clone()
        };
        Some(build_list_url(&base_url, 1, None, search.as_deref(), Some(&next), &where_params))
    } else {
        None
    };
    let title_sorted_asc = title_field.as_ref().map(|tf| {
        let sf = sort.as_deref().map(|s| s.strip_prefix('-').unwrap_or(s));
        let desc = sort.as_deref().map(|s| s.starts_with('-')).unwrap_or(false);
        sf == Some(tf.as_str()) && !desc
    }).unwrap_or(false);
    let title_sorted_desc = title_field.as_ref().map(|tf| {
        let sf = sort.as_deref().map(|s| s.strip_prefix('-').unwrap_or(s));
        let desc = sort.as_deref().map(|s| s.starts_with('-')).unwrap_or(false);
        sf == Some(tf.as_str()) && desc
    }).unwrap_or(false);

    let is_upload = def.is_upload_collection();
    let admin_thumbnail = def.upload.as_ref()
        .and_then(|u| u.admin_thumbnail.as_ref().cloned());
    let items: Vec<_> = documents.iter().map(|doc| {
        let title_value = title_field.as_ref()
            .and_then(|f| doc.get_str(f))
            .unwrap_or_else(|| {
                if is_upload {
                    doc.get_str("filename").unwrap_or(&doc.id)
                } else {
                    &doc.id
                }
            });

        let cells = compute_cells(doc, &table_columns, &def);

        let mut item = serde_json::json!({
            "id": doc.id,
            "title_value": title_value,
            "created_at": doc.created_at,
            "updated_at": doc.updated_at,
            "cells": cells,
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

    // Build pagination URLs preserving sort + where params
    let prev_url = build_list_url(&base_url, page - 1, None, search.as_deref(), sort.as_deref(), &where_params);
    let next_url = build_list_url(&base_url, page + 1, None, search.as_deref(), sort.as_deref(), &where_params);

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionItems, def.display_name())
        .set("page_title", serde_json::json!(def.display_name()))
        .collection_def(&def)
        .items(items)
        .pagination(page, per_page, total, prev_url, next_url)
        .set("has_drafts", serde_json::json!(def.has_drafts()))
        .set("search", serde_json::json!(search))
        .set("sort", serde_json::json!(sort))
        .set("table_columns", serde_json::json!(table_columns))
        .set("column_options", serde_json::json!(column_options))
        .set("filter_fields", serde_json::json!(filter_fields))
        .set("active_filters", serde_json::json!(filter_pills))
        .set("active_filter_count", serde_json::json!(filter_pills.len()))
        .set("title_sort_url", serde_json::json!(title_sort_url))
        .set("title_sorted_asc", serde_json::json!(title_sorted_asc))
        .set("title_sorted_desc", serde_json::json!(title_sorted_desc))
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/items", &data).into_response()
}

/// POST /admin/api/user-settings/{slug} — save user column preferences
pub async fn save_user_settings(
    State(state): State<AdminState>,
    Path(collection_slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    axum::Form(form): axum::Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let auth_user = match auth_user {
        Some(Extension(au)) => au,
        None => return axum::http::StatusCode::UNAUTHORIZED.into_response(),
    };

    // Validate collection exists
    let def = match state.registry.get_collection(&collection_slug) {
        Some(d) => d.clone(),
        None => return axum::http::StatusCode::NOT_FOUND.into_response(),
    };

    // Parse columns from form (comma-separated or multiple "columns" fields)
    let columns: Vec<String> = form.get("columns")
        .map(|c| c.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    // Validate column keys
    let valid_columns: Vec<String> = columns.into_iter()
        .filter(|k| {
            k == "created_at" || k == "updated_at" || k == "_status"
                || def.fields.iter().any(|f| f.name == *k && is_column_eligible(&f.field_type))
        })
        .collect();

    // Load existing settings, merge, save
    let pool = state.pool.clone();
    let user_id = auth_user.claims.sub.clone();
    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get().context("Failed to get DB connection")?;
        let existing = query::auth::get_user_settings(&conn, &user_id)?;
        let mut settings: serde_json::Value = existing
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        settings[&collection_slug] = serde_json::json!({ "columns": valid_columns });

        let json_str = serde_json::to_string(&settings)?;
        query::auth::set_user_settings(&conn, &user_id, &json_str)?;
        Ok::<_, anyhow::Error>(())
    }).await;

    match result {
        Ok(Ok(())) => axum::http::StatusCode::NO_CONTENT.into_response(),
        _ => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Collect hidden upload field values from form data for re-rendering after validation errors.
/// Returns a JSON array of `{ "name": "...", "value": "..." }` objects for hidden `<input>` elements.
fn collect_upload_hidden_fields(
    fields: &[crate::core::field::FieldDefinition],
    form_data: &HashMap<String, String>,
) -> serde_json::Value {
    let hidden_fields: Vec<serde_json::Value> = fields.iter()
        .filter(|f| f.admin.hidden)
        .filter_map(|f| {
            form_data.get(&f.name).map(|v| {
                serde_json::json!({"name": &f.name, "value": v})
            })
        })
        .collect();
    serde_json::json!(hidden_fields)
}

/// GET /admin/collections/{slug}/create — show create form
pub async fn create_form(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
    };

    // Check create access
    match check_access_or_forbid(
        &state, def.access.create.as_deref(), &auth_user, None, None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to create items in this collection").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let non_default_locale = is_non_default_locale(&state, editor_locale.as_deref());
    let mut fields = build_field_contexts(&def.fields, &HashMap::new(), &HashMap::new(), true, non_default_locale);

    // Enrich relationship and array fields
    enrich_field_contexts(&mut fields, &def.fields, &HashMap::new(), &state, true, non_default_locale, &HashMap::new(), None);

    // Evaluate display conditions (empty form data for create)
    let empty_data = serde_json::json!({});
    apply_display_conditions(&mut fields, &def.fields, &empty_data, &state.hook_runner, true);

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

    // Split fields into main and sidebar
    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    let (_locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let mut data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionCreate, format!("Create {}", def.singular_name()))
        .set("page_title", serde_json::json!(format!("Create {}", def.singular_name())))
        .collection_def(&def)
        .fields(main_fields)
        .set("sidebar_fields", serde_json::json!(sidebar_fields))
        .set("editing", serde_json::json!(false))
        .set("has_drafts", serde_json::json!(def.has_drafts()))
        .breadcrumbs(vec![
            Breadcrumb::link("Collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current(format!("Create {}", def.singular_name())),
        ])
        .merge(locale_data)
        .build();

    // Add upload context for upload collections
    if def.is_upload_collection() {
        let mut upload_ctx = serde_json::json!({});
        if let Some(ref u) = def.upload {
            if !u.mime_types.is_empty() {
                upload_ctx["accept"] = serde_json::json!(u.mime_types.join(","));
            }
        }
        data["upload"] = upload_ctx;
    }

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/edit", &data).into_response()
}

/// POST /admin/collections/{slug} — create a new item
pub async fn create_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
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
    let mut queued_conversions = Vec::new();
    if let Some(f) = file {
        if let Some(ref upload_config) = def.upload {
            match upload::process_upload(
                &f, upload_config, &state.config_dir, &slug,
                state.config.upload.max_file_size,
            ) {
                Ok(processed) => {
                    queued_conversions = processed.queued_conversions.clone();
                    inject_upload_metadata(&mut form_data, &processed);
                }
                Err(e) => {
                    tracing::error!("Upload processing error: {}", e);
                    let mut fields = build_field_contexts(&def.fields, &form_data, &HashMap::new(), true, false);
                    enrich_field_contexts(&mut fields, &def.fields, &HashMap::new(), &state, true, false, &HashMap::new(), None);
                    let empty_data = serde_json::json!({});
                    apply_display_conditions(&mut fields, &def.fields, &empty_data, &state.hook_runner, true);
                    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);
                    let data = ContextBuilder::new(&state, None)
                        .locale_from_auth(&auth_user)
                        .page(PageType::CollectionCreate, format!("Create {}", def.singular_name()))
                        .set("page_title", serde_json::json!(format!("Create {}", def.singular_name())))
                        .collection_def(&def)
                        .fields(main_fields)
                        .set("sidebar_fields", serde_json::json!(sidebar_fields))
                        .set("editing", serde_json::json!(false))
                        .set("has_drafts", serde_json::json!(def.has_drafts()))
                        .build();
                    return html_with_toast(&state, "collections/edit", &data, &e.to_string());
                }
            }
        }
    }

    // Strip field-level create-denied fields (skip pool.get if no field-level access configured)
    if def.fields.iter().any(|f| f.access.create.is_some()) {
        let user_doc = get_user_doc(&auth_user);
        if let Ok(mut conn) = state.pool.get() {
            if let Ok(tx) = conn.transaction() {
                let denied = state.hook_runner.check_field_write_access(&def.fields, user_doc, "create", &tx);
                let _ = tx.commit();
                for name in &denied {
                    form_data.remove(name);
                }
            }
        }
    }

    // Extract password before it enters hooks/regular data flow
    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };

    // Convert comma-separated multi-select values to JSON arrays
    transform_select_has_many(&mut form_data, &def.fields);

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
    let join_data_clone = join_data.clone();
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
        Ok(Ok((doc, _req_context))) => {
            // Enqueue deferred image conversions if any
            if !queued_conversions.is_empty() {
                if let Ok(conn) = state.pool.get() {
                    if let Err(e) = upload::enqueue_conversions(&conn, &slug, &doc.id, &queued_conversions) {
                        tracing::warn!("Failed to enqueue image conversions: {}", e);
                    }
                }
            }

            state.hook_runner.publish_event(
                &state.event_bus, &def.hooks, def.live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Create,
                slug.clone(), doc.id.clone(), doc.fields.clone(),
                get_event_user(&auth_user),
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

            htmx_redirect(&format!("/admin/collections/{}", slug))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let error_map = ve.to_field_map();
                let mut fields = build_field_contexts(&def.fields, &form_data_clone, &error_map, true, false);
                enrich_field_contexts(&mut fields, &def.fields, &join_data_clone, &state, true, false, &error_map, None);
                let form_json = serde_json::json!(form_data_clone.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect::<serde_json::Map<String, serde_json::Value>>());
                apply_display_conditions(&mut fields, &def.fields, &form_json, &state.hook_runner, true);
                let (main_fields, sidebar_fields) = split_sidebar_fields(fields);
                let mut data = ContextBuilder::new(&state, None)
                    .locale_from_auth(&auth_user)
                    .page(PageType::CollectionCreate, format!("Create {}", def.singular_name()))
                    .set("page_title", serde_json::json!(format!("Create {}", def.singular_name())))
                    .collection_def(&def)
                    .fields(main_fields)
                    .set("sidebar_fields", serde_json::json!(sidebar_fields))
                    .set("editing", serde_json::json!(false))
                    .set("has_drafts", serde_json::json!(def.has_drafts()))
                    .build();
                // Preserve upload metadata as hidden inputs so they survive form re-submission
                if def.is_upload_collection() {
                    data["upload_hidden_fields"] = collect_upload_hidden_fields(&def.fields, &form_data_clone);
                }
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
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
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

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let (locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

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
    let has_drafts = def.has_drafts();
    let read_result = tokio::task::spawn_blocking(move || {
        runner.fire_before_read(&hooks, &slug_owned, "find_by_id", HashMap::new())?;
        let conn = pool.get().context("DB connection")?;
        let mut doc = ops::find_by_id_full(
            &conn, &slug_owned, &def_owned, &id_owned,
            locale_ctx.as_ref(), access_constraints, has_drafts,
        )?;
        // Assemble sizes for upload collections
        if let Some(ref mut d) = doc {
            if let Some(ref upload_config) = def_owned.upload {
                if upload_config.enabled {
                    upload::assemble_sizes_object(d, upload_config);
                }
            }
        }
        let doc = doc.map(|d| runner.apply_after_read(&hooks, &fields, &slug_owned, "find_by_id", d));
        Ok::<_, anyhow::Error>(doc)
    }).await;

    let mut document = match read_result {
        Ok(Ok(Some(doc))) => doc,
        Ok(Ok(None)) => return not_found(&state, &format!("Document '{}' not found", id)).into_response(),
        Ok(Err(e)) => return server_error(&state, &format!("Query error: {}", e)).into_response(),
        Err(e) => return server_error(&state, &format!("Task error: {}", e)).into_response(),
    };

    // Strip field-level read-denied fields (skip pool.get if no field-level access configured)
    if def.fields.iter().any(|f| f.access.read.is_some()) {
        let user_doc = get_user_doc(&auth_user);
        if let Ok(mut conn) = state.pool.get() {
            if let Ok(tx) = conn.transaction() {
                let denied = state.hook_runner.check_field_read_access(&def.fields, user_doc, &tx);
                let _ = tx.commit();
                strip_denied_fields(&mut document.fields, &denied);
            }
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

    let non_default_locale = is_non_default_locale(&state, editor_locale.as_deref());
    let mut fields = build_field_contexts(&def.fields, &values, &HashMap::new(), true, non_default_locale);

    // Enrich relationship and array fields with extra data
    enrich_field_contexts(&mut fields, &def.fields, &document.fields, &state, true, non_default_locale, &HashMap::new(), Some(&id));

    // Evaluate display conditions with document data
    let form_data_json = serde_json::json!(document.fields);
    apply_display_conditions(&mut fields, &def.fields, &form_data_json, &state.hook_runner, true);

    if def.is_auth_collection() {
        fields.push(serde_json::json!({
            "name": "password",
            "field_type": "password",
            "label": "Password",
            "required": false,
            "value": "",
            "description": "Leave blank to keep current password",
        }));

        // Add locked checkbox — read current lock state from DB
        let is_locked = state.pool.get().ok()
            .and_then(|conn| query::auth::is_locked(&conn, &slug, &id).ok())
            .unwrap_or(false);
        fields.push(serde_json::json!({
            "name": "_locked",
            "field_type": "checkbox",
            "label": "Account locked",
            "checked": is_locked,
            "description": "Prevent this user from logging in",
        }));
    }

    // Split fields into main and sidebar
    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    // Determine document title for breadcrumb
    let doc_title = def.title_field()
        .and_then(|f| document.get_str(f))
        .map(|s| s.to_string())
        .unwrap_or_else(|| document.id.clone());

    // Fetch document status and version history for versioned collections
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
        fetch_version_sidebar_data(&state.pool, &slug, &document.id)
    } else {
        (vec![], 0)
    };

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let mut data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionEdit, format!("Edit {}", def.singular_name()))
        .set("page_title", serde_json::json!(format!("Edit {}", def.singular_name())))
        .collection_def(&def)
        .document_with_status(&document, &doc_status)
        .fields(main_fields)
        .set("sidebar_fields", serde_json::json!(sidebar_fields))
        .set("editing", serde_json::json!(true))
        .set("has_drafts", serde_json::json!(has_drafts))
        .set("has_versions", serde_json::json!(has_versions))
        .set("versions", serde_json::json!(versions))
        .set("has_more_versions", serde_json::json!(total_versions > 3))
        .set("restore_url_prefix", serde_json::json!(format!("/admin/collections/{}/{}", slug, id)))
        .set("versions_url", serde_json::json!(format!("/admin/collections/{}/{}/versions", slug, id)))
        .breadcrumbs(vec![
            Breadcrumb::link("Collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current(doc_title),
        ])
        .merge(locale_data)
        .build();

    // Add upload context for upload collections
    if def.is_upload_collection() {
        let mut upload_ctx = serde_json::json!({});
        if let Some(ref u) = def.upload {
            if !u.mime_types.is_empty() {
                upload_ctx["accept"] = serde_json::json!(u.mime_types.join(","));
            }
        }

        // Upload preview and file info from existing document
        let url = document.fields.get("url").and_then(|v| v.as_str());
        let mime_type = document.fields.get("mime_type").and_then(|v| v.as_str());
        let filename = document.fields.get("filename").and_then(|v| v.as_str());
        let filesize = document.fields.get("filesize").and_then(|v| v.as_f64()).map(|v| v as u64);
        let width = document.fields.get("width").and_then(|v| v.as_f64()).map(|v| v as u32);
        let height = document.fields.get("height").and_then(|v| v.as_f64()).map(|v| v as u32);

        // Pass focal point values
        let focal_x = document.fields.get("focal_x").and_then(|v| v.as_f64());
        let focal_y = document.fields.get("focal_y").and_then(|v| v.as_f64());
        if let Some(fx) = focal_x {
            upload_ctx["focal_x"] = serde_json::json!(fx);
        }
        if let Some(fy) = focal_y {
            upload_ctx["focal_y"] = serde_json::json!(fy);
        }

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
                upload_ctx["preview"] = serde_json::json!(preview_url);
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
            upload_ctx["info"] = info;
        }
        data["upload"] = upload_ctx;
    }

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/edit", &data).into_response()
}

/// POST handler for update/delete (HTML forms use _method override).
pub async fn update_action_post(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
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
    let def = match state.registry.get_collection(slug) {
        Some(d) => d.clone(),
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
    let mut queued_conversions = Vec::new();
    if let Some(f) = file {
        if let Some(ref upload_config) = def.upload {
            // Load old document to get old file paths for cleanup
            if let Ok(conn) = state.pool.get() {
                if let Ok(Some(old_doc)) = query::find_by_id(&conn, slug, &def, id, locale_ctx.as_ref()) {
                    old_doc_fields = Some(old_doc.fields.clone());
                }
            }

            match upload::process_upload(
                &f, upload_config, &state.config_dir, slug,
                state.config.upload.max_file_size,
            ) {
                Ok(processed) => {
                    queued_conversions = processed.queued_conversions.clone();
                    inject_upload_metadata(&mut form_data, &processed);
                }
                Err(e) => {
                    tracing::error!("Upload processing error: {}", e);
                    let mut fields = build_field_contexts(&def.fields, &form_data, &HashMap::new(), true, false);
                    enrich_field_contexts(&mut fields, &def.fields, &HashMap::new(), state, true, false, &HashMap::new(), Some(&id));
                    let form_json = serde_json::json!(form_data.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect::<serde_json::Map<String, serde_json::Value>>());
                    apply_display_conditions(&mut fields, &def.fields, &form_json, &state.hook_runner, true);
                    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);
                    let data = ContextBuilder::new(state, None)
                        .locale_from_auth(auth_user)
                        .page(PageType::CollectionEdit, format!("Edit {}", def.singular_name()))
                        .set("page_title", serde_json::json!(format!("Edit {}", def.singular_name())))
                        .collection_def(&def)
                        .document_stub(id)
                        .fields(main_fields)
                        .set("sidebar_fields", serde_json::json!(sidebar_fields))
                        .set("editing", serde_json::json!(true))
                        .set("has_drafts", serde_json::json!(def.has_drafts()))
                        .build();
                    return html_with_toast(state, "collections/edit", &data, &e.to_string());
                }
            }
        }
    }

    // Strip field-level update-denied fields (skip pool.get if no field-level access configured)
    if def.fields.iter().any(|f| f.access.update.is_some()) {
        let user_doc = get_user_doc(auth_user);
        if let Ok(mut conn) = state.pool.get() {
            if let Ok(tx) = conn.transaction() {
                let denied = state.hook_runner.check_field_write_access(&def.fields, user_doc, "update", &tx);
                let _ = tx.commit();
                for name in &denied {
                    form_data.remove(name);
                }
            }
        }
    }

    // Extract password and locked before they enter hooks/regular data flow
    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };
    let locked_value = if def.is_auth_collection() {
        Some(form_data.remove("_locked"))
    } else {
        None
    };

    // Convert comma-separated multi-select values to JSON arrays
    transform_select_has_many(&mut form_data, &def.fields);

    // Extract join table data (arrays + has-many relationships) from form
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.to_string();
    let id_owned = id.to_string();
    let def_owned = def.clone();
    let form_data_clone = form_data.clone();
    let join_data_clone = join_data.clone();
    let user_doc = get_user_doc(auth_user).cloned();
    let locale = locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        query::LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let action_owned = action.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Handle unpublish: set _status to 'draft' and create a version
        let result = if action_owned == "unpublish" && def_owned.has_versions() {
            let doc = crate::service::unpublish_document(
                &pool, &runner, &slug_owned, &id_owned, &def_owned, user_doc.as_ref(),
            )?;
            Ok((doc, HashMap::new()))
        } else {
            crate::service::update_document(
                &pool, &runner, &slug_owned, &id_owned, &def_owned,
                form_data, &join_data,
                password.as_deref(), locale_ctx.as_ref(), locale,
                user_doc.as_ref(), draft,
            )
        };

        // Update lock status for auth collections (after successful update)
        if result.is_ok() {
            if let Some(locked_field) = locked_value {
                let should_lock = locked_field.as_deref() == Some("on")
                    || locked_field.as_deref() == Some("1");
                let conn = pool.get().context("DB connection for lock update")?;
                if should_lock {
                    query::auth::lock_user(&conn, &slug_owned, &id_owned)?;
                } else {
                    query::auth::unlock_user(&conn, &slug_owned, &id_owned)?;
                }
            }
        }

        result
    }).await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            // If a new file was uploaded and old files exist, clean up old files
            if let Some(old_fields) = old_doc_fields {
                upload::delete_upload_files(&state.config_dir, &old_fields);
            }

            // Enqueue deferred image conversions if any
            if !queued_conversions.is_empty() {
                if let Ok(conn) = state.pool.get() {
                    if let Err(e) = upload::enqueue_conversions(&conn, slug, id, &queued_conversions) {
                        tracing::warn!("Failed to enqueue image conversions: {}", e);
                    }
                }
            }

            state.hook_runner.publish_event(
                &state.event_bus, &def.hooks, def.live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Update,
                slug.to_string(), id.to_string(), doc.fields.clone(),
                get_event_user(auth_user),
            );
            htmx_redirect(&format!("/admin/collections/{}/{}", slug, id))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let error_map = ve.to_field_map();
                let mut fields = build_field_contexts(&def.fields, &form_data_clone, &error_map, true, false);
                enrich_field_contexts(&mut fields, &def.fields, &join_data_clone, state, true, false, &error_map, Some(&id));
                let form_json = serde_json::json!(form_data_clone.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect::<serde_json::Map<String, serde_json::Value>>());
                apply_display_conditions(&mut fields, &def.fields, &form_json, &state.hook_runner, true);
                let (main_fields, sidebar_fields) = split_sidebar_fields(fields);
                let mut data = ContextBuilder::new(state, None)
                    .locale_from_auth(auth_user)
                    .page(PageType::CollectionEdit, format!("Edit {}", def.singular_name()))
                    .set("page_title", serde_json::json!(format!("Edit {}", def.singular_name())))
                    .collection_def(&def)
                    .document_stub(id)
                    .fields(main_fields)
                    .set("sidebar_fields", serde_json::json!(sidebar_fields))
                    .set("editing", serde_json::json!(true))
                    .set("has_drafts", serde_json::json!(def.has_drafts()))
                    .build();
                // Preserve upload metadata as hidden inputs so they survive form re-submission
                if def.is_upload_collection() {
                    data["upload_hidden_fields"] = collect_upload_hidden_fields(&def.fields, &form_data_clone);
                }
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
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
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

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionDelete, format!("Delete {}", def.singular_name()))
        .set("page_title", serde_json::json!(format!("Delete {}", def.singular_name())))
        .collection_def(&def)
        .set("document_id", serde_json::json!(id))
        .set("title_value", serde_json::json!(title_value))
        .breadcrumbs(vec![
            Breadcrumb::link("Collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current(format!("Delete {}", def.singular_name())),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);

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
    let def = match state.registry.get_collection(slug) {
        Some(d) => d.clone(),
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

    // Before hooks + delete + upload cleanup in a single transaction
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let def_clone = def.clone();
    let slug_owned = slug.to_string();
    let id_owned = id.to_string();
    let user_doc = get_user_doc(auth_user).cloned();
    let config_dir = state.config_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::service::delete_document(
            &pool, &runner, &slug_owned, &id_owned, &def_clone, user_doc.as_ref(),
            Some(&config_dir),
        )
    }).await;

    match result {
        Ok(Ok(_req_context)) => {
            state.hook_runner.publish_event(
                &state.event_bus, &def.hooks, def.live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Delete,
                slug.to_string(), id.to_string(), std::collections::HashMap::new(),
                get_event_user(auth_user),
            );
        }
        Ok(Err(e)) => {
            tracing::error!("Delete error: {}", e);
        }
        Err(e) => {
            tracing::error!("Delete task error: {}", e);
        }
    }

    htmx_redirect(&format!("/admin/collections/{}", slug))
}

/// POST /admin/collections/{slug}/{id}/versions/{version_id}/restore — restore a version
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, id, version_id)): Path<(String, String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
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
        Ok(Ok(_)) => htmx_redirect(&format!("/admin/collections/{}/{}", slug, id)),
        Ok(Err(e)) => {
            tracing::error!("Restore version error: {}", e);
            htmx_redirect(&format!("/admin/collections/{}/{}", slug, id))
        }
        Err(e) => {
            tracing::error!("Restore version task error: {}", e);
            htmx_redirect(&format!("/admin/collections/{}/{}", slug, id))
        }
    }
}

/// GET /admin/collections/{slug}/{id}/versions — dedicated version history page
pub async fn list_versions_page(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
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

    // Build locale context so localized column names resolve correctly
    let locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale);

    // Fetch the document for breadcrumb title
    let document = match ops::find_document_by_id(&state.pool, &slug, &def, &id, locale_ctx.as_ref()) {
        Ok(Some(doc)) => doc,
        Ok(None) => return not_found(&state, &format!("Document '{}' not found", id)).into_response(),
        Err(e) => return server_error(&state, &format!("Query error: {}", e)).into_response(),
    };

    let doc_title = def.title_field()
        .and_then(|f| document.get_str(f))
        .map(|s| s.to_string())
        .unwrap_or_else(|| document.id.clone());

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page
        .unwrap_or(state.config.pagination.default_limit)
        .min(state.config.pagination.max_limit);
    let offset = (page - 1) * per_page;

    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return server_error(&state, "Database error").into_response(),
    };

    let total = query::count_versions(&conn, &slug, &id).unwrap_or(0);
    let versions: Vec<serde_json::Value> = query::list_versions(&conn, &slug, &id, Some(per_page), Some(offset))
        .unwrap_or_default()
        .into_iter()
        .map(version_to_json)
        .collect();

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionVersions, format!("Version History — {}", doc_title))
        .set("page_title", serde_json::json!(format!("Version History — {}", doc_title)))
        .collection_def(&def)
        .document_stub(&id)
        .set("doc_title", serde_json::json!(doc_title))
        .set("versions", serde_json::json!(versions))
        .set("restore_url_prefix", serde_json::json!(format!("/admin/collections/{}/{}", slug, id)))
        .pagination(
            page, per_page, total,
            format!("/admin/collections/{}/{}/versions?page={}", slug, id, page - 1),
            format!("/admin/collections/{}/{}/versions?page={}", slug, id, page + 1),
        )
        .breadcrumbs(vec![
            Breadcrumb::link("Collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::link(doc_title.clone(), format!("/admin/collections/{}/{}", slug, id)),
            Breadcrumb::current("Version History"),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/versions", &data).into_response()
}

/// POST /admin/collections/{slug}/evaluate-conditions
/// Evaluates server-only display conditions with current form data.
/// Returns JSON: { "field_name": true/false, ... }
pub async fn evaluate_conditions(
    State(state): State<AdminState>,
    Path(_slug): Path<String>,
    axum::Json(req): axum::Json<EvaluateConditionsRequest>,
) -> impl IntoResponse {
    use crate::hooks::lifecycle::DisplayConditionResult;

    let form_data = serde_json::json!(req.form_data);
    let mut results = serde_json::Map::new();
    for (field_name, func_ref) in &req.conditions {
        let visible = match state.hook_runner.call_display_condition(func_ref, &form_data) {
            Some(DisplayConditionResult::Bool(b)) => b,
            Some(DisplayConditionResult::Table { visible, .. }) => visible,
            None => true, // error → show
        };
        results.insert(field_name.clone(), serde_json::json!(visible));
    }
    axum::Json(serde_json::Value::Object(results))
}

#[derive(serde::Deserialize)]
pub struct EvaluateConditionsRequest {
    form_data: HashMap<String, serde_json::Value>,
    conditions: HashMap<String, String>,
}

#[derive(serde::Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    limit: Option<usize>,
}

/// GET /admin/api/search/{collection}?q=...&limit=20
/// Returns JSON array of `{id, label}` for relationship field search.
pub async fn search_collection(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    axum::extract::Query(params): axum::extract::Query<SearchQuery>,
) -> impl IntoResponse {
    let Some(def) = state.registry.get_collection(&slug) else {
        return axum::Json(serde_json::json!([]));
    };
    let title_field = def.title_field().map(|s| s.to_string());
    let search_term = params.q.unwrap_or_default().to_lowercase();
    let limit = params.limit
        .unwrap_or(state.config.pagination.default_limit as usize)
        .min(state.config.pagination.max_limit as usize);

    let is_upload = def.upload.as_ref().is_some_and(|u| u.enabled);
    let admin_thumbnail = def.upload.as_ref()
        .and_then(|u| u.admin_thumbnail.as_ref().cloned());

    let Ok(conn) = state.pool.get() else {
        return axum::Json(serde_json::json!([]));
    };

    let locale_ctx = crate::db::query::LocaleContext::from_locale_string(
        None, &state.config.locale,
    );
    let find_query = query::FindQuery {
        limit: Some(limit as i64),
        search: if search_term.is_empty() { None } else { Some(search_term.clone()) },
        ..Default::default()
    };
    let Ok(mut docs) = query::find(&conn, &slug, def, &find_query, locale_ctx.as_ref()) else {
        return axum::Json(serde_json::json!([]));
    };

    // Assemble sizes for upload collections so we can extract thumbnail URLs
    if is_upload {
        if let Some(ref upload_config) = def.upload {
            for doc in &mut docs {
                upload::assemble_sizes_object(doc, upload_config);
            }
        }
    }

    let results: Vec<_> = docs.iter()
        .map(|doc| {
            let label = if is_upload {
                doc.get_str("filename")
                    .or_else(|| title_field.as_ref().and_then(|f| doc.get_str(f)))
                    .unwrap_or(&doc.id)
                    .to_string()
            } else {
                title_field.as_ref()
                    .and_then(|f| doc.get_str(f))
                    .unwrap_or(&doc.id)
                    .to_string()
            };
            let mut item = serde_json::json!({ "id": doc.id, "label": label });
            if is_upload {
                let mime = doc.get_str("mime_type").unwrap_or("");
                let is_image = mime.starts_with("image/");
                let thumb_url = if is_image {
                    admin_thumbnail.as_ref()
                        .and_then(|thumb_name| {
                            doc.fields.get("sizes")
                                .and_then(|v| v.get(thumb_name))
                                .and_then(|v| v.get("url"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .or_else(|| doc.get_str("url").map(|s| s.to_string()))
                } else { None };
                if let Some(url) = thumb_url { item["thumbnail_url"] = serde_json::json!(url); }
                item["filename"] = serde_json::json!(label);
                if is_image { item["is_image"] = serde_json::json!(true); }
            }
            item
        })
        .collect();

    axum::Json(serde_json::json!(results))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldAdmin, FieldDefinition, FieldType, SelectOption, LocalizedString};
    use crate::core::collection::*;

    fn test_collection() -> CollectionDefinition {
        CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "title".to_string(),
                    field_type: FieldType::Text,
                    ..Default::default()
                },
                FieldDefinition {
                    name: "status".to_string(),
                    field_type: FieldType::Select,
                    options: vec![
                        SelectOption { label: LocalizedString::Plain("Draft".into()), value: "draft".into() },
                        SelectOption { label: LocalizedString::Plain("Published".into()), value: "published".into() },
                    ],
                    ..Default::default()
                },
                FieldDefinition {
                    name: "body".to_string(),
                    field_type: FieldType::Richtext,
                    ..Default::default()
                },
                FieldDefinition {
                    name: "views".to_string(),
                    field_type: FieldType::Number,
                    ..Default::default()
                },
                FieldDefinition {
                    name: "active".to_string(),
                    field_type: FieldType::Checkbox,
                    ..Default::default()
                },
                FieldDefinition {
                    name: "date".to_string(),
                    field_type: FieldType::Date,
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin {
                use_as_title: Some("title".to_string()),
                ..Default::default()
            },
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        }
    }

    // --- field_label tests ---

    #[test]
    fn field_label_uses_admin_label() {
        let f = FieldDefinition {
            name: "my_field".to_string(),
            admin: FieldAdmin {
                label: Some(LocalizedString::Plain("Custom Label".into())),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(field_label(&f), "Custom Label");
    }

    #[test]
    fn field_label_falls_back_to_name() {
        let f = FieldDefinition {
            name: "my_field".to_string(),
            ..Default::default()
        };
        assert_eq!(field_label(&f), "My Field");
    }

    // --- resolve_columns tests ---

    #[test]
    fn resolve_columns_defaults() {
        let def = test_collection();
        let cols = resolve_columns(&def, None, None, "/admin/collections/posts", "", None);
        // Default: created_at (no _status since no drafts)
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0]["key"], "created_at");
    }

    #[test]
    fn resolve_columns_user_cols() {
        let def = test_collection();
        let user_cols = vec!["status".to_string(), "views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), None, "/admin/collections/posts", "", None);
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0]["key"], "status");
        assert_eq!(cols[1]["key"], "views");
    }

    #[test]
    fn resolve_columns_filters_invalid() {
        let def = test_collection();
        let user_cols = vec!["title".to_string(), "body".to_string(), "views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), None, "/admin/collections/posts", "", None);
        // title is stripped (it's the title field), body is Richtext (ineligible)
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0]["key"], "views");
    }

    #[test]
    fn resolve_columns_sort_state() {
        let def = test_collection();
        let user_cols = vec!["views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), Some("views"), "/admin/collections/posts", "", None);
        assert_eq!(cols[0]["is_sorted_asc"], true);
        assert_eq!(cols[0]["is_sorted_desc"], false);
    }

    #[test]
    fn resolve_columns_sort_desc_state() {
        let def = test_collection();
        let user_cols = vec!["views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), Some("-views"), "/admin/collections/posts", "", None);
        assert_eq!(cols[0]["is_sorted_asc"], false);
        assert_eq!(cols[0]["is_sorted_desc"], true);
    }

    // --- compute_cells tests ---

    #[test]
    fn compute_cells_status_badge() {
        let def = test_collection();
        let mut doc = crate::core::Document {
            id: "1".into(),
            fields: HashMap::new(),
            created_at: None,
            updated_at: None,
        };
        doc.fields.insert("_status".into(), serde_json::json!("draft"));

        let columns = vec![serde_json::json!({"key": "_status"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_badge"], true);
        assert_eq!(cells[0]["value"], "draft");
    }

    #[test]
    fn compute_cells_select_shows_label() {
        let def = test_collection();
        let mut doc = crate::core::Document {
            id: "1".into(),
            fields: HashMap::new(),
            created_at: None,
            updated_at: None,
        };
        doc.fields.insert("status".into(), serde_json::json!("published"));

        let columns = vec![serde_json::json!({"key": "status"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["value"], "Published");
    }

    #[test]
    fn compute_cells_checkbox() {
        let def = test_collection();
        let mut doc = crate::core::Document {
            id: "1".into(),
            fields: HashMap::new(),
            created_at: None,
            updated_at: None,
        };
        doc.fields.insert("active".into(), serde_json::json!(1));

        let columns = vec![serde_json::json!({"key": "active"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_bool"], true);
        assert_eq!(cells[0]["value"], true);
    }

    #[test]
    fn compute_cells_date() {
        let def = test_collection();
        let doc = crate::core::Document {
            id: "1".into(),
            fields: HashMap::new(),
            created_at: Some("2024-01-15".into()),
            updated_at: None,
        };

        let columns = vec![serde_json::json!({"key": "created_at"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_date"], true);
    }

    // --- build_column_options tests ---

    #[test]
    fn build_column_options_includes_eligible() {
        let def = test_collection();
        let opts = build_column_options(&def, &["status".to_string()]);
        // Should have system cols + eligible fields (not title, not body/richtext)
        let keys: Vec<&str> = opts.iter().filter_map(|o| o["key"].as_str()).collect();
        assert!(keys.contains(&"created_at"));
        assert!(keys.contains(&"updated_at"));
        assert!(keys.contains(&"status")); // select - eligible
        assert!(keys.contains(&"views")); // number - eligible
        assert!(!keys.contains(&"body")); // richtext - ineligible
        assert!(!keys.contains(&"title")); // title field - excluded
    }

    #[test]
    fn build_column_options_marks_selected() {
        let def = test_collection();
        let opts = build_column_options(&def, &["status".to_string()]);
        let status_opt = opts.iter().find(|o| o["key"] == "status").unwrap();
        assert_eq!(status_opt["selected"], true);
        let views_opt = opts.iter().find(|o| o["key"] == "views").unwrap();
        assert_eq!(views_opt["selected"], false);
    }

    // --- build_filter_fields tests ---

    #[test]
    fn build_filter_fields_includes_eligible() {
        let def = test_collection();
        let fields = build_filter_fields(&def);
        let keys: Vec<&str> = fields.iter().filter_map(|f| f["key"].as_str()).collect();
        assert!(keys.contains(&"created_at"));
        assert!(keys.contains(&"status"));
        assert!(keys.contains(&"views"));
        assert!(!keys.contains(&"body")); // richtext ineligible
    }

    #[test]
    fn build_filter_fields_select_has_options() {
        let def = test_collection();
        let fields = build_filter_fields(&def);
        let status_field = fields.iter().find(|f| f["key"] == "status").unwrap();
        let opts = status_field["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "draft");
    }

    // --- op_to_param_name tests ---

    #[test]
    fn op_to_param_name_all_ops() {
        assert_eq!(op_to_param_name(&FilterOp::Equals("x".into())), "equals");
        assert_eq!(op_to_param_name(&FilterOp::NotEquals("x".into())), "not_equals");
        assert_eq!(op_to_param_name(&FilterOp::Contains("x".into())), "contains");
        assert_eq!(op_to_param_name(&FilterOp::GreaterThan("x".into())), "gt");
        assert_eq!(op_to_param_name(&FilterOp::LessThan("x".into())), "lt");
        assert_eq!(op_to_param_name(&FilterOp::GreaterThanOrEqual("x".into())), "gte");
        assert_eq!(op_to_param_name(&FilterOp::LessThanOrEqual("x".into())), "lte");
        assert_eq!(op_to_param_name(&FilterOp::Exists), "exists");
        assert_eq!(op_to_param_name(&FilterOp::NotExists), "not_exists");
    }

    #[test]
    fn collect_upload_hidden_fields_basic() {
        let fields = vec![
            FieldDefinition { name: "filename".to_string(), ..Default::default() },
            FieldDefinition { name: "mime_type".to_string(), admin: FieldAdmin { hidden: true, ..Default::default() }, ..Default::default() },
            FieldDefinition { name: "url".to_string(), admin: FieldAdmin { hidden: true, ..Default::default() }, ..Default::default() },
            FieldDefinition { name: "width".to_string(), field_type: FieldType::Number, admin: FieldAdmin { hidden: true, ..Default::default() }, ..Default::default() },
            FieldDefinition { name: "alt".to_string(), ..Default::default() },
        ];
        let mut form_data = HashMap::new();
        form_data.insert("filename".to_string(), "test.jpg".to_string());
        form_data.insert("mime_type".to_string(), "image/jpeg".to_string());
        form_data.insert("url".to_string(), "/uploads/media/test.jpg".to_string());
        form_data.insert("width".to_string(), "1920".to_string());
        form_data.insert("alt".to_string(), "Test".to_string());

        let result = collect_upload_hidden_fields(&fields, &form_data);
        let arr = result.as_array().unwrap();

        // Only hidden fields: mime_type, url, width (not filename or alt — they're not hidden)
        assert_eq!(arr.len(), 3);
        assert!(arr.iter().any(|f| f["name"] == "mime_type" && f["value"] == "image/jpeg"));
        assert!(arr.iter().any(|f| f["name"] == "url" && f["value"] == "/uploads/media/test.jpg"));
        assert!(arr.iter().any(|f| f["name"] == "width" && f["value"] == "1920"));
    }

    #[test]
    fn collect_upload_hidden_fields_missing_values() {
        let fields = vec![
            FieldDefinition { name: "url".to_string(), admin: FieldAdmin { hidden: true, ..Default::default() }, ..Default::default() },
            FieldDefinition { name: "mime_type".to_string(), admin: FieldAdmin { hidden: true, ..Default::default() }, ..Default::default() },
        ];
        // Only url is in form_data, not mime_type
        let mut form_data = HashMap::new();
        form_data.insert("url".to_string(), "/uploads/media/test.jpg".to_string());

        let result = collect_upload_hidden_fields(&fields, &form_data);
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "url");
    }

    #[test]
    fn collect_upload_hidden_fields_no_hidden() {
        let fields = vec![
            FieldDefinition { name: "alt".to_string(), ..Default::default() },
        ];
        let form_data = HashMap::new();
        let result = collect_upload_hidden_fields(&fields, &form_data);
        assert_eq!(result.as_array().unwrap().len(), 0);
    }
}
