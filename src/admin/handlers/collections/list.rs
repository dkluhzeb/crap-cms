//! Collection list handlers: list collections, list items, save user settings.

use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    Extension,
};
use anyhow::Context as _;
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType};
use crate::core::auth::{AuthUser, Claims};
use crate::core::field::FieldType;
use crate::core::upload;
use crate::db::query;
use crate::db::query::{AccessResult, FindQuery, FilterOp, FilterClause, LocaleContext};

use super::{
    PaginationParams,
    get_user_doc, check_access_or_forbid, extract_editor_locale,
    auto_label_from_name, url_decode,
    parse_where_params, validate_sort, build_list_url, is_column_eligible,
    extract_where_params,
    forbidden, render_or_error, not_found, server_error,
};

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
                .map(field_label)
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

    let mut find_query = FindQuery::new();
    find_query.filters = filters.clone();
    find_query.order_by = order_by;
    find_query.limit = Some(per_page);
    find_query.offset = Some(offset);
    find_query.search = search.clone();

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
        Ok(Err(e)) => { tracing::error!("Collection list query error: {}", e); return server_error(&state, "An internal error occurred.").into_response(); }
        Err(e) => { tracing::error!("Collection list task error: {}", e); return server_error(&state, "An internal error occurred.").into_response(); }
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
        // Read-only access check — commit result is irrelevant, rollback on drop is safe
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldAdmin, FieldDefinition, FieldType, SelectOption, LocalizedString};
    use crate::core::collection::*;
    use crate::core::document::DocumentBuilder;

    fn test_collection() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition {
                name: "title".to_string(),
                field_type: FieldType::Text,
                ..Default::default()
            },
            FieldDefinition {
                name: "status".to_string(),
                field_type: FieldType::Select,
                options: vec![
                    SelectOption::new(LocalizedString::Plain("Draft".into()), "draft"),
                    SelectOption::new(LocalizedString::Plain("Published".into()), "published"),
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
        ];
        def.admin = CollectionAdmin {
            use_as_title: Some("title".to_string()),
            ..Default::default()
        };
        def
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
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields.insert("_status".into(), serde_json::json!("draft"));

        let columns = vec![serde_json::json!({"key": "_status"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_badge"], true);
        assert_eq!(cells[0]["value"], "draft");
    }

    #[test]
    fn compute_cells_select_shows_label() {
        let def = test_collection();
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields.insert("status".into(), serde_json::json!("published"));

        let columns = vec![serde_json::json!({"key": "status"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["value"], "Published");
    }

    #[test]
    fn compute_cells_checkbox() {
        let def = test_collection();
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields.insert("active".into(), serde_json::json!(1));

        let columns = vec![serde_json::json!({"key": "active"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_bool"], true);
        assert_eq!(cells[0]["value"], true);
    }

    #[test]
    fn compute_cells_date() {
        let def = test_collection();
        let doc = DocumentBuilder::new("1").created_at("2024-01-15").build();

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
}
