use axum::{
    Extension,
    extract::{Path, Query, State},
    http::{HeaderMap, Uri},
    response::Response,
};
use serde_json::{Value, from_str, json};
use tracing::{error, warn};

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::{
            collections::shared::{
                build_column_options, build_filter_fields, build_filter_pills, compute_cells,
                resolve_columns, thumbnail_url,
            },
            shared::{
                ListUrlContext, PaginationParams, extract_editor_locale, extract_where_params,
                forbidden, not_found, parse_where_params, paths, render_or_error, server_error,
                validate_sort,
            },
        },
    },
    core::{
        CollectionDefinition, Document,
        auth::{AuthUser, Claims},
    },
    db::query::{self, FilterClause, FindQuery, LocaleContext},
    service::{
        FindDocumentsInput, PaginatedResult, RunnerReadHooks, ServiceContext, ServiceError,
        find_documents, user_settings::get_user_settings,
    },
};

/// Fetch documents via the shared service layer read lifecycle.
///
/// `is_trash` is a presentation flag from the request — the service layer
/// injects `_deleted_at EXISTS` and flips `include_deleted` itself. The admin
/// list view always wants to see drafts alongside published rows (admins
/// manage both), so `include_drafts` is set unconditionally.
/// Arguments for [`fetch_list_documents`]. All fields are required; constructed
/// at the single call site in [`list_items`] — plain struct literal per
/// CLAUDE.md's "single call site" exception to the builder rule.
struct FetchListArgs<'a> {
    state: &'a AdminState,
    slug: &'a str,
    def: &'a CollectionDefinition,
    find_query: &'a FindQuery,
    locale_ctx: Option<&'a query::LocaleContext>,
    auth_user: &'a Option<Extension<AuthUser>>,
    cursor_enabled: bool,
    is_trash: bool,
}

fn fetch_list_documents(
    args: FetchListArgs<'_>,
) -> Result<PaginatedResult<Document>, ServiceError> {
    let conn = args.state.pool.get().map_err(ServiceError::Internal)?;
    let user_doc = args
        .auth_user
        .as_ref()
        .map(|Extension(au)| au.user_doc.clone());

    let hooks = RunnerReadHooks::new(&args.state.hook_runner, &conn);
    let ctx = ServiceContext::collection(args.slug, args.def)
        .pool(&args.state.pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user_doc.as_ref())
        .build();

    let input = FindDocumentsInput::builder(args.find_query)
        .hydrate(false)
        .locale_ctx(args.locale_ctx)
        .cursor_enabled(args.cursor_enabled)
        .trash(args.is_trash)
        .include_drafts(true)
        .build();

    find_documents(&ctx, &input)
}

/// Compute title column sort URL and sort direction indicators.
fn compute_title_sort(
    def: &CollectionDefinition,
    url_ctx: &ListUrlContext,
) -> (Option<String>, bool, bool) {
    let title_field = match def.title_field() {
        Some(tf) => tf.to_string(),
        None => return (None, false, false),
    };

    let sort_field_name = url_ctx.sort.map(|s| s.strip_prefix('-').unwrap_or(s));
    let sort_desc = url_ctx.sort.map(|s| s.starts_with('-')).unwrap_or(false);
    let is_sorted = sort_field_name == Some(title_field.as_str());

    let next = if is_sorted && !sort_desc {
        format!("-{}", title_field)
    } else {
        title_field.clone()
    };

    (
        Some(url_ctx.sort_url(&next)),
        is_sorted && !sort_desc,
        is_sorted && sort_desc,
    )
}

/// Pagination context for the list view.
struct ListPagination {
    result: query::PaginationResult,
    prev_url: String,
    next_url: String,
}

/// Build prev/next URLs from the PaginationResult for cursor or page mode.
fn build_list_pagination(
    pr: &query::PaginationResult,
    pagination: &query::FindPagination,
    cursor_enabled: bool,
    url_ctx: &ListUrlContext,
) -> ListPagination {
    let (prev_url, next_url) = if cursor_enabled {
        let prev = if pr.has_prev_page {
            pr.start_cursor
                .as_deref()
                .map(|sc| url_ctx.cursor_url("before_cursor", sc))
                .unwrap_or_default()
        } else {
            String::new()
        };

        let next = if pr.has_next_page {
            pr.end_cursor
                .as_deref()
                .map(|ec| url_ctx.cursor_url("after_cursor", ec))
                .unwrap_or_default()
        } else {
            String::new()
        };

        (prev, next)
    } else {
        (
            url_ctx.page_url(pagination.page - 1),
            url_ctx.page_url(pagination.page + 1),
        )
    };

    ListPagination {
        result: pr.clone(),
        prev_url,
        next_url,
    }
}

/// Build the FindQuery from pagination, user filters, sort, and search params.
///
/// Produces a *user* query — system filters (`_deleted_at`, `_status`) are
/// injected by `service::find_documents` based on the typed flags. The trash
/// default sort (`-_deleted_at`) is set by the caller as a presentation choice.
fn build_find_query(
    pagination: &query::FindPagination,
    url_filters: &[FilterClause],
    order_by: Option<String>,
    search: Option<&str>,
) -> FindQuery {
    let offset = (!pagination.has_cursor()).then_some(pagination.offset);

    FindQuery::builder()
        .filters(url_filters.to_vec())
        .order_by(order_by)
        .limit(Some(pagination.limit))
        .offset(offset)
        .after_cursor(pagination.after_cursor.clone())
        .before_cursor(pagination.before_cursor.clone())
        .search(search.map(str::to_string))
        .build()
}

/// Load the user's saved column preferences for a collection.
fn load_user_columns(
    state: &AdminState,
    auth_user: &Option<Extension<AuthUser>>,
    slug: &str,
) -> Option<Vec<String>> {
    let Extension(au) = auth_user.as_ref()?;
    let conn = state.pool.get().ok()?;
    let settings_json = get_user_settings(&conn, &au.claims.sub).ok()??;
    let settings: Value = from_str(&settings_json).ok()?;
    let cols = settings.get(slug)?.get("columns")?.as_array()?;

    Some(
        cols.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
    )
}

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

    let is_trash = def.soft_delete && params.trash.as_deref() == Some("1");

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
            warn!("Invalid pagination params: {}", e);

            return server_error(&state, "Invalid pagination parameters");
        }
    };

    let sort = params.sort.as_deref().and_then(|s| validate_sort(s, &def));
    let url_filters = parse_where_params(raw_query, &def);

    let order_by = if is_trash {
        Some("-_deleted_at".to_string())
    } else {
        sort.clone().or_else(|| def.admin.default_sort.clone())
    };

    let find_query = build_find_query(
        &pagination,
        &url_filters,
        order_by.clone(),
        search.as_deref(),
    );

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let locale_ctx =
        LocaleContext::from_locale_string(editor_locale.as_deref(), &state.config.locale)
            .unwrap_or(None);

    let state_clone = state.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let auth_user_clone = auth_user.clone();

    let read_result = tokio::task::spawn_blocking(move || {
        fetch_list_documents(FetchListArgs {
            state: &state_clone,
            slug: &slug_owned,
            def: &def_owned,
            find_query: &find_query,
            locale_ctx: locale_ctx.as_ref(),
            auth_user: &auth_user_clone,
            cursor_enabled,
            is_trash,
        })
    })
    .await;

    let result = match read_result {
        Ok(Ok(v)) => v,
        Ok(Err(ServiceError::AccessDenied(_))) => {
            return forbidden(
                &state,
                if is_trash {
                    "You don't have permission to view the trash"
                } else {
                    "You don't have permission to view this collection"
                },
            );
        }
        Ok(Err(e)) => {
            error!("Collection list query error: {}", e);

            return server_error(&state, "An internal error occurred.");
        }
        Err(e) => {
            error!("Collection list task error: {}", e);

            return server_error(&state, "An internal error occurred.");
        }
    };

    let pagination_result = result.pagination;
    let documents = result.docs;

    let user_columns = load_user_columns(&state, &auth_user, &slug);

    let base_url = paths::collection(&slug);
    let mut where_params = extract_where_params(raw_query);

    if is_trash {
        if where_params.is_empty() {
            where_params = "trash=1".to_string();
        } else {
            where_params = format!("trash=1&{}", where_params);
        }
    }

    let url_ctx = ListUrlContext {
        base_url: &base_url,
        search: search.as_deref(),
        sort: sort.as_deref(),
        where_params: &where_params,
    };

    let table_columns = resolve_columns(&def, user_columns.as_deref(), &url_ctx);
    let column_keys: Vec<String> = table_columns
        .iter()
        .filter_map(|c| c["key"].as_str().map(|s| s.to_string()))
        .collect();
    let column_options = build_column_options(&def, &column_keys);
    let filter_fields = build_filter_fields(&def);
    let filter_pills = build_filter_pills(&url_filters, &def, raw_query);

    let (title_sort_url, title_sorted_asc, title_sorted_desc) = compute_title_sort(&def, &url_ctx);

    let items: Vec<_> = documents
        .iter()
        .map(|doc| build_item_row(doc, &table_columns, &def))
        .collect();

    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let ctx = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .filter_nav_by_access(&state, &auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionItems, def.display_name())
        .collection_def(&def)
        .docs(items);

    let lp = build_list_pagination(&pagination_result, &pagination, cursor_enabled, &url_ctx);

    let ctx = ctx.with_pagination(&lp.result, lp.prev_url, lp.next_url);

    let data = ctx
        .set("has_drafts", json!(def.has_drafts()))
        .set("has_soft_delete", json!(def.soft_delete))
        .set("is_trash", json!(is_trash))
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

    if is_upload {
        let admin_thumb = def
            .upload
            .as_ref()
            .and_then(|u| u.admin_thumbnail.as_deref());

        if let Some(url) = thumbnail_url(doc, admin_thumb) {
            item["thumbnail_url"] = json!(url);
        }
    }

    item
}
