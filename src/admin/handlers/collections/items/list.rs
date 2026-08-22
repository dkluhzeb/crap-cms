use axum::{
    Extension,
    extract::{Path, Query, State},
    http::{HeaderMap, Uri},
    response::Response,
};
use serde_json::{Value, from_str, json};
use tracing::warn;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, CollectionContext, CollectionPermissions, PageMeta, PageType,
            PaginationContext, page::collections::CollectionItemsListPage,
        },
        handlers::{
            collections::shared::{
                build_column_options, build_filter_fields, build_filter_pills, compute_cells,
                resolve_columns, thumbnail_url,
            },
            shared::{
                ListUrlContext, PaginationParams, bad_request, extract_editor_locale,
                extract_status_filter, extract_where_params, parse_where_params, paths,
                render_page, require_collection, service_error_to_admin_response,
                task_join_error_response, validate_sort,
            },
        },
    },
    core::{AuthUser, Claims, CollectionDefinition, Document},
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
/// list view shows drafts alongside published rows for users who can view them
/// (edit-level access); a read-only viewer sees published only (see
/// `fetch_list_documents`).
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
    status_filter: Option<Vec<String>>,
}

fn fetch_list_documents(
    args: FetchListArgs<'_>,
) -> Result<PaginatedResult<Document>, ServiceError> {
    let conn = args.state.pool.get().map_err(ServiceError::Internal)?;
    let user_doc = args
        .auth_user
        .as_ref()
        .map(|Extension(au)| au.user_doc.clone());

    let hooks = RunnerReadHooks::new(&args.state.hook_runner, &conn, user_doc.as_ref(), None);
    let ctx = ServiceContext::collection(args.slug, args.def)
        .pool(&args.state.pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user_doc.as_ref())
        .build();

    // Request drafts unconditionally — the service read path returns the union
    // of the views the caller may see and downgrades (never rejects): an editor
    // gets published + drafts, a read-only admin gets published only. Draft
    // *visibility* is the service's job; `CollectionPermissions` survives only
    // as a UI hint (show/hide the Drafts tab), not a request gate.
    let input = FindDocumentsInput::builder(args.find_query)
        .hydrate(false)
        .locale_ctx(args.locale_ctx)
        .cursor_enabled(args.cursor_enabled)
        .trash(args.is_trash)
        .include_drafts(true)
        .status_filter(args.status_filter)
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
    let sort_desc = url_ctx.sort.is_some_and(|s| s.starts_with('-'));
    let is_sorted = sort_field_name == Some(title_field.as_str());

    let next = if is_sorted && !sort_desc {
        format!("-{title_field}")
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

/// Build prev/next URLs from the `PaginationResult` for cursor or page mode.
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

/// Build the `FindQuery` from pagination, user filters, sort, and search params.
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
    auth_user: Option<&Extension<AuthUser>>,
    slug: &str,
) -> Option<Vec<String>> {
    let Extension(au) = auth_user?;
    let conn = state.pool.get().ok()?;
    let settings_json = get_user_settings(&conn, &au.claims.sub).ok()??;
    let settings: Value = from_str(&settings_json).ok()?;
    let cols = settings.get(slug)?.get("columns")?.as_array()?;

    Some(
        cols.iter()
            .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
            .collect(),
    )
}

/// Validated + normalized inputs derived from the request URL/headers,
/// produced by [`parse_list_inputs`]. All downstream phases (fetch +
/// view-model build + page-context build) take this as a single
/// reference — keeps the handler a thin orchestrator.
struct ListInputs {
    is_trash: bool,
    cursor_enabled: bool,
    search: Option<String>,
    pagination: query::FindPagination,
    sort: Option<String>,
    url_filters: Vec<FilterClause>,
    status_filter: Option<Vec<String>>,
    find_query: FindQuery,
    editor_locale: Option<String>,
    locale_ctx: Option<LocaleContext>,
    raw_query: String,
}

/// Parse the query string + headers + pagination params into a typed
/// [`ListInputs`] bundle. Returns a response (not an error) for the
/// invalid-pagination case so the handler can short-circuit cleanly.
fn parse_list_inputs(
    state: &AdminState,
    def: &CollectionDefinition,
    params: PaginationParams,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<ListInputs, Box<Response>> {
    let is_trash = def.soft_delete && params.trash.as_deref() == Some("1");
    let raw_query = uri.query().unwrap_or("").to_string();
    let cursor_enabled = state.config.pagination.is_cursor();
    let search = params.search.filter(|s| !s.trim().is_empty());

    let pg_ctx = query::PaginationCtx::from_config(&state.config.pagination);
    let pagination = pg_ctx
        .validate(
            params.per_page,
            params.page,
            params.after_cursor.as_deref(),
            params.before_cursor.as_deref(),
        )
        .inspect_err(|e| warn!("Invalid pagination params: {}", e))
        .map_err(|_| Box::new(bad_request(state, "Invalid pagination parameters")))?;

    // Present-but-invalid URL params hard-error with 400 (parity with the
    // MCP/gRPC surfaces) instead of silently rendering wrong/unfiltered or
    // default-sorted results.
    let sort = match params.sort.as_deref() {
        None => None,
        Some(s) => Some(validate_sort(s, def).ok_or_else(|| {
            Box::new(bad_request(
                state,
                &format!("Unknown or unsortable sort field '{s}'"),
            ))
        })?),
    };

    let url_filters =
        parse_where_params(&raw_query, def).map_err(|e| Box::new(bad_request(state, &e)))?;

    // The filter UI exposes `_status` for collections with drafts (see
    // `build_filter_fields`); the URL it produces (`where[_status][equals]=X`,
    // including OR-bucket forms) is handled here as a typed param rather
    // than a generic where clause, because system columns (`_*`) are
    // off-limits to user filters at the service layer
    // (`validate_user_filters`). See `extract_status_filter` for the
    // parsing rule. Multiple values widen to `_status IN (...)` at
    // injection time.
    let status_filter = match extract_status_filter(&raw_query) {
        None => None,
        Some(values) => {
            if !def.has_drafts() {
                return Err(Box::new(bad_request(
                    state,
                    "Status filter is not available on this collection (drafts are disabled)",
                )));
            }

            if let Some(bad) = values.iter().find(|s| *s != "draft" && *s != "published") {
                return Err(Box::new(bad_request(
                    state,
                    &format!("Unknown status filter value '{bad}' (valid: draft, published)"),
                )));
            }

            Some(values)
        }
    };

    let order_by = if is_trash {
        Some(query::TRASH_DEFAULT_ORDER.to_string())
    } else {
        sort.clone().or_else(|| def.admin.default_sort.clone())
    };

    let find_query = build_find_query(&pagination, &url_filters, order_by, search.as_deref());

    let editor_locale = extract_editor_locale(headers, &state.config.locale);
    let locale_ctx =
        LocaleContext::from_locale_string(editor_locale.as_deref(), &state.config.locale)
            .unwrap_or(None);

    Ok(ListInputs {
        is_trash,
        cursor_enabled,
        search,
        pagination,
        sort,
        url_filters,
        status_filter,
        find_query,
        editor_locale,
        locale_ctx,
        raw_query,
    })
}

/// Run the `find_documents` read pipeline on the blocking pool and map
/// the service error / join error to an admin-rendered response.
async fn fetch_list_items(
    state: AdminState,
    slug: String,
    def: CollectionDefinition,
    inputs: &ListInputs,
    auth_user: Option<Extension<AuthUser>>,
) -> Result<PaginatedResult<Document>, Response> {
    let state_for_blocking = state.clone();
    let find_query = inputs.find_query.clone();
    let locale_ctx = inputs.locale_ctx.clone();
    let status_filter = inputs.status_filter.clone();
    let cursor_enabled = inputs.cursor_enabled;
    let is_trash = inputs.is_trash;

    let read_result = tokio::task::spawn_blocking(move || {
        fetch_list_documents(FetchListArgs {
            state: &state_for_blocking,
            slug: &slug,
            def: &def,
            find_query: &find_query,
            locale_ctx: locale_ctx.as_ref(),
            auth_user: &auth_user,
            cursor_enabled,
            is_trash,
            status_filter,
        })
    })
    .await;

    match read_result {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => {
            let denied_msg = if is_trash {
                "You don't have permission to view the trash"
            } else {
                "You don't have permission to view this collection"
            };
            Err(service_error_to_admin_response(&state, e, denied_msg))
        }
        Err(e) => Err(task_join_error_response(&state, &e)),
    }
}

/// Inputs to [`build_list_page`]. All fields required; constructed at the
/// single call site in [`list_items`] — plain struct literal per CLAUDE.md.
struct BuildListPageInput<'a> {
    state: &'a AdminState,
    slug: &'a str,
    def: &'a CollectionDefinition,
    inputs: ListInputs,
    fetched: PaginatedResult<Document>,
    claims: Option<&'a Claims>,
    auth_user: Option<&'a Extension<AuthUser>>,
}

/// Assemble the typed `CollectionItemsListPage` view-model from the
/// parsed inputs and the fetched documents. Encapsulates all the
/// table/column/filter/title-sort plumbing that previously sat inline
/// in the handler.
fn build_list_page(args: BuildListPageInput<'_>) -> CollectionItemsListPage {
    let BuildListPageInput {
        state,
        slug,
        def,
        inputs,
        fetched,
        claims,
        auth_user,
    } = args;
    let documents = fetched.docs;
    let pagination_result = fetched.pagination;
    let user_columns = load_user_columns(state, auth_user, slug);

    let base_url = paths::collection(slug);
    let mut where_params = extract_where_params(&inputs.raw_query);

    if inputs.is_trash {
        if where_params.is_empty() {
            where_params = "trash=1".to_string();
        } else {
            where_params = format!("trash=1&{where_params}");
        }
    }

    let url_ctx = ListUrlContext {
        base_url: &base_url,
        search: inputs.search.as_deref(),
        sort: inputs.sort.as_deref(),
        where_params: &where_params,
    };

    let table_columns = resolve_columns(def, user_columns.as_deref(), &url_ctx);
    let column_keys: Vec<String> = table_columns
        .iter()
        .filter_map(|c| c["key"].as_str().map(std::string::ToString::to_string))
        .collect();
    let column_options = build_column_options(def, &column_keys);
    let filter_fields = build_filter_fields(def);
    let filter_pills = build_filter_pills(&inputs.url_filters, def, &inputs.raw_query);

    let (title_sort_url, title_sorted_asc, title_sorted_desc) = compute_title_sort(def, &url_ctx);

    let items: Vec<_> = documents
        .iter()
        .map(|doc| build_item_row(doc, &table_columns, def))
        .collect();

    let lp = build_list_pagination(
        &pagination_result,
        &inputs.pagination,
        inputs.cursor_enabled,
        &url_ctx,
    );

    let base = BasePageContext::for_handler(
        state,
        claims,
        auth_user,
        PageMeta::new(PageType::CollectionItems, def.display_name()),
    )
    .with_editor_locale(inputs.editor_locale.as_deref(), state);

    let active_filter_count = filter_pills.len();
    let perms = CollectionPermissions::for_user(state, def, auth_user);

    CollectionItemsListPage {
        base,
        collection: CollectionContext::from_def(def),
        perms,
        docs: items,
        pagination: PaginationContext::from_result(&lp.result, lp.prev_url, lp.next_url),
        has_drafts: def.has_drafts(),
        has_soft_delete: def.soft_delete,
        is_trash: inputs.is_trash,
        search: inputs.search,
        sort: inputs.sort,
        table_columns,
        column_options,
        filter_fields,
        active_filters: filter_pills,
        active_filter_count,
        title_sort_url,
        title_sorted_asc,
        title_sorted_desc,
    }
}

/// GET /admin/collections/{slug} — list items in a collection.
///
/// Thin orchestrator: resolve the collection definition, parse query
/// inputs, fetch documents, build the typed view-model, render.
pub async fn list_items(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Query(params): Query<PaginationParams>,
    uri: Uri,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match require_collection(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    let inputs = match parse_list_inputs(&state, &def, params, &uri, &headers) {
        Ok(i) => i,
        Err(resp) => return *resp,
    };

    let fetched = match fetch_list_items(
        state.clone(),
        slug.clone(),
        def.clone(),
        &inputs,
        auth_user.clone(),
    )
    .await
    {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let ctx = build_list_page(BuildListPageInput {
        state: &state,
        slug: &slug,
        def: &def,
        inputs,
        fetched,
        claims: claims_ref,
        auth_user: auth_user.as_ref(),
    });

    render_page(&state, "collections/items", &ctx)
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

#[cfg(test)]
mod tests {
    use crate::admin::handlers::query::url::ListUrlContext;

    use super::*;

    fn titled_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.admin.use_as_title = Some("title".to_string());
        def
    }

    fn ctx(sort: Option<&'static str>) -> ListUrlContext<'static> {
        ListUrlContext {
            base_url: "/admin/collections/posts",
            search: None,
            sort,
            where_params: "",
        }
    }

    #[test]
    fn no_title_field_yields_no_sort() {
        let (url, asc, desc) = compute_title_sort(&CollectionDefinition::new("posts"), &ctx(None));
        assert!(url.is_none());
        assert!(!asc && !desc);
    }

    #[test]
    fn unsorted_offers_ascending_toggle() {
        let (url, asc, desc) = compute_title_sort(&titled_def(), &ctx(None));
        assert!(url.is_some());
        assert!(!asc && !desc); // not currently sorted by title
    }

    #[test]
    fn ascending_active_next_toggles_to_descending() {
        let (url, asc, desc) = compute_title_sort(&titled_def(), &ctx(Some("title")));
        assert!(asc && !desc);
        // next link flips to descending.
        assert!(url.unwrap().contains("sort=-title"));
    }

    #[test]
    fn descending_active_next_toggles_back_to_ascending() {
        let (url, asc, desc) = compute_title_sort(&titled_def(), &ctx(Some("-title")));
        assert!(desc && !asc);
        assert!(url.unwrap().contains("sort=title"));
    }
}
