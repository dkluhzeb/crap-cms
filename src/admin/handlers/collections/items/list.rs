use anyhow::Context as _;
use axum::{
    Extension,
    extract::{Path, Query, State},
    http::{HeaderMap, Uri},
    response::Response,
};
use serde_json::{Value, from_str, json};
use std::collections::HashMap;

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::{
            collections::shared::{
                build_column_options, build_filter_fields, build_filter_pills, compute_cells,
                resolve_columns,
            },
            shared::{
                PaginationParams, build_list_url, build_list_url_with_cursor,
                check_access_or_forbid, compute_denied_read_fields, extract_editor_locale,
                extract_where_params, forbidden, not_found, parse_where_params, render_or_error,
                server_error, validate_sort,
            },
        },
    },
    core::{
        CollectionDefinition, Document,
        auth::{AuthUser, Claims},
        upload,
    },
    db::query::{self, AccessResult, FilterClause, FindQuery, LocaleContext},
    hooks::lifecycle::AfterReadCtx,
};

/// GET /admin/collections/{slug} — list items in a collection
pub async fn list_items(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Query(params): Query<PaginationParams>,
    uri: Uri,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return not_found(&state, &format!("Collection '{}' not found", slug));
        }
    };

    // Check read access
    let access_result =
        match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
            Ok(r) => r,
            Err(resp) => return *resp,
        };
    if matches!(access_result, AccessResult::Denied) {
        return forbidden(&state, "You don't have permission to view this collection");
    }

    let raw_query = uri.query().unwrap_or("");
    let cursor_enabled = state.config.pagination.is_cursor();
    let search = params.search.filter(|s| !s.trim().is_empty());

    let pg_ctx = query::PaginationCtx::new(
        state.config.pagination.default_limit,
        state.config.pagination.max_limit,
        cursor_enabled,
    );
    let pagination = match pg_ctx.validate(
        params.per_page,
        params.page,
        params.after_cursor.as_deref(),
        params.before_cursor.as_deref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Invalid pagination params: {}", e);
            return server_error(&state, "Invalid pagination parameters");
        }
    };

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
    find_query.order_by = order_by.clone();
    find_query.limit = Some(pagination.limit);
    find_query.offset = if pagination.has_cursor() {
        None
    } else {
        Some(pagination.offset)
    };
    find_query.after_cursor = pagination.after_cursor.clone();
    find_query.before_cursor = pagination.before_cursor.clone();
    find_query.search = search.clone();

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let locale_ctx =
        LocaleContext::from_locale_string(editor_locale.as_deref(), &state.config.locale);

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let fields = def.fields.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let user_doc = auth_user.as_ref().map(|Extension(au)| au.user_doc.clone());
    let user_ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());
    let read_result = tokio::task::spawn_blocking(move || {
        runner.fire_before_read(&hooks, &slug_owned, "find", HashMap::new())?;
        let conn = pool.get().context("Failed to get DB connection")?;

        let total = query::count_with_search(
            &conn,
            &slug_owned,
            &def_owned,
            &filters,
            locale_ctx.as_ref(),
            find_query.search.as_deref(),
        )?;

        let mut docs = query::find(
            &conn,
            &slug_owned,
            &def_owned,
            &find_query,
            locale_ctx.as_ref(),
        )?;

        // Assemble sizes for upload collections
        if let Some(ref upload_config) = def_owned.upload
            && upload_config.enabled
        {
            for doc in &mut docs {
                upload::assemble_sizes_object(doc, upload_config);
            }
        }

        let ar_ctx = AfterReadCtx {
            hooks: &hooks,
            fields: &fields,
            collection: &slug_owned,
            operation: "find",
            user: user_doc.as_ref(),
            ui_locale: user_ui_locale.as_deref(),
        };
        let docs = runner.apply_after_read_many(&ar_ctx, docs);

        Ok::<_, anyhow::Error>((docs, total))
    })
    .await;

    let (documents, total) = match read_result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            tracing::error!("Collection list query error: {}", e);
            return server_error(&state, "An internal error occurred.");
        }
        Err(e) => {
            tracing::error!("Collection list task error: {}", e);
            return server_error(&state, "An internal error occurred.");
        }
    };

    // Strip field-level read-denied fields from documents
    let denied_fields = match compute_denied_read_fields(&state, &auth_user, &def.fields) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    let documents: Vec<_> = documents
        .into_iter()
        .map(|mut doc| {
            for field_name in &denied_fields {
                doc.fields.remove(field_name);
            }
            doc
        })
        .collect();

    // Load user column preferences
    let user_columns: Option<Vec<String>> = auth_user.as_ref().and_then(|Extension(au)| {
        let conn = state.pool.get().ok()?;
        let settings_json = query::auth::get_user_settings(&conn, &au.claims.sub).ok()??;
        let settings: Value = from_str(&settings_json).ok()?;
        let cols = settings.get(&slug)?.get("columns")?.as_array()?;

        Some(
            cols.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
        )
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
    let column_keys: Vec<String> = table_columns
        .iter()
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

        Some(build_list_url(
            &base_url,
            1,
            None,
            search.as_deref(),
            Some(&next),
            &where_params,
        ))
    } else {
        None
    };

    let title_sorted_asc = title_field
        .as_ref()
        .map(|tf| {
            let sf = sort.as_deref().map(|s| s.strip_prefix('-').unwrap_or(s));
            let desc = sort.as_deref().map(|s| s.starts_with('-')).unwrap_or(false);
            sf == Some(tf.as_str()) && !desc
        })
        .unwrap_or(false);
    let title_sorted_desc = title_field
        .as_ref()
        .map(|tf| {
            let sf = sort.as_deref().map(|s| s.strip_prefix('-').unwrap_or(s));
            let desc = sort.as_deref().map(|s| s.starts_with('-')).unwrap_or(false);
            sf == Some(tf.as_str()) && desc
        })
        .unwrap_or(false);

    let items: Vec<_> = documents
        .iter()
        .map(|doc| build_item_row(doc, &table_columns, &def))
        .collect();

    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    // Build pagination URLs and context based on mode
    let ctx = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionItems, def.display_name())
        .collection_def(&def)
        .docs(items);

    let pr = if cursor_enabled {
        query::PaginationResult::builder(&documents, total, pagination.limit).cursor(
            order_by.as_deref(),
            def.timestamps,
            pagination.before_cursor.is_some(),
            pagination.has_cursor(),
        )
    } else {
        query::PaginationResult::builder(&documents, total, pagination.limit)
            .page(pagination.page, pagination.offset)
    };

    let (prev_url, next_url) = if cursor_enabled {
        let prev = if pr.has_prev_page {
            pr.start_cursor
                .as_deref()
                .map(|sc| {
                    build_list_url_with_cursor(
                        &base_url,
                        1,
                        None,
                        search.as_deref(),
                        sort.as_deref(),
                        &where_params,
                        Some(("before_cursor", sc)),
                    )
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        let next = if pr.has_next_page {
            pr.end_cursor
                .as_deref()
                .map(|ec| {
                    build_list_url_with_cursor(
                        &base_url,
                        1,
                        None,
                        search.as_deref(),
                        sort.as_deref(),
                        &where_params,
                        Some(("after_cursor", ec)),
                    )
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        (prev, next)
    } else {
        let prev = build_list_url(
            &base_url,
            pagination.page - 1,
            None,
            search.as_deref(),
            sort.as_deref(),
            &where_params,
        );
        let next = build_list_url(
            &base_url,
            pagination.page + 1,
            None,
            search.as_deref(),
            sort.as_deref(),
            &where_params,
        );
        (prev, next)
    };

    let ctx = ctx.with_pagination(&pr, prev_url, next_url);

    let data = ctx
        .set("has_drafts", json!(def.has_drafts()))
        .set("search", json!(search))
        .set("sort", json!(sort))
        .set("table_columns", json!(table_columns))
        .set("column_options", json!(column_options))
        .set("filter_fields", json!(filter_fields))
        .set("active_filters", json!(filter_pills))
        .set("active_filter_count", json!(filter_pills.len()))
        .set("title_sort_url", json!(title_sort_url))
        .set("title_sorted_asc", json!(title_sorted_asc))
        .set("title_sorted_desc", json!(title_sorted_desc))
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/items", &data)
}

/// Build a single item row for the collection list table.
fn build_item_row(doc: &Document, table_columns: &[Value], def: &CollectionDefinition) -> Value {
    let is_upload = def.is_upload_collection();
    let title_field = def.title_field();

    let title_value = title_field.and_then(|f| doc.get_str(f)).unwrap_or_else(|| {
        if is_upload {
            doc.get_str("filename").unwrap_or(&doc.id)
        } else {
            &doc.id
        }
    });

    let cells = compute_cells(doc, table_columns, def);

    let mut item = json!({
        "id": doc.id,
        "title_value": title_value,
        "created_at": doc.created_at,
        "updated_at": doc.updated_at,
        "cells": cells,
    });

    // Add thumbnail URL for upload collections
    if is_upload {
        let admin_thumbnail = def
            .upload
            .as_ref()
            .and_then(|u| u.admin_thumbnail.as_deref());
        let mime = doc.get_str("mime_type").unwrap_or("");

        if mime.starts_with("image/") {
            let thumb_url = admin_thumbnail
                .and_then(|thumb_name| {
                    doc.fields
                        .get("sizes")
                        .and_then(|v| v.get(thumb_name))
                        .and_then(|v| v.get("url"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .or_else(|| doc.get_str("url").map(|s| s.to_string()));

            if let Some(url) = thumb_url {
                item["thumbnail_url"] = json!(url);
            }
        }
    }

    item
}
