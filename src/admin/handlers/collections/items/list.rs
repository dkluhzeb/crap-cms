use axum::{
    Extension,
    extract::{Path, Query, State},
    http::{HeaderMap, Uri},
    response::Response,
};
use serde_json::{Value, json};

use super::{
    list_fetch::{FetchedList, fetch_list_items},
    list_inputs::{ListInputs, ListRequest, parse_list_inputs},
};
use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, CollectionContext, CollectionPermissions, PageMeta, PageType,
            PaginationContext, page::collections::CollectionItemsListPage,
        },
        handlers::{
            collections::shared::{
                FilterPillInputs, ListFieldAccess, active_filter_count, build_column_options,
                build_filter_fields, build_filter_pills, compute_cells, resolve_columns,
                thumbnail_url, title_label,
            },
            shared::{
                HxNav, ListUrlContext, PageRequest, PaginationParams, extract_where_params, paths,
                render_page, require_collection, ui_locale_of,
            },
        },
    },
    core::{AuthUser, Claims, CollectionDefinition, Document},
    db::query,
};

/// Compute title column sort URL and sort direction indicators. No sort link
/// when the collection has no title field or the viewer is not offered it.
fn compute_title_sort(
    def: &CollectionDefinition,
    url_ctx: &ListUrlContext,
    access: &ListFieldAccess,
) -> (Option<String>, bool, bool) {
    let Some(title_field) = def.title_field() else {
        return (None, false, false);
    };

    if title_label(def, access).is_none() {
        return (None, false, false);
    }

    let sort_field_name = url_ctx.sort.map(|s| s.strip_prefix('-').unwrap_or(s));
    let sort_desc = url_ctx.sort.is_some_and(|s| s.starts_with('-'));
    let is_sorted = sort_field_name == Some(title_field);

    let next = if is_sorted && !sort_desc {
        format!("-{title_field}")
    } else {
        title_field.to_string()
    };

    (
        Some(url_ctx.sort_url(&next)),
        is_sorted && !sort_desc,
        is_sorted && sort_desc,
    )
}

/// Build the pagination context (prev/next URLs) for cursor or page mode.
fn build_list_pagination(
    pr: &query::PaginationResult,
    pagination: &query::FindPagination,
    cursor_enabled: bool,
    url_ctx: &ListUrlContext,
) -> PaginationContext {
    let (prev_url, next_url) = if cursor_enabled {
        let prev = pr
            .start_cursor
            .as_deref()
            .filter(|_| pr.has_prev_page)
            .map(|sc| url_ctx.cursor_url("before_cursor", sc))
            .unwrap_or_default();

        let next = pr
            .end_cursor
            .as_deref()
            .filter(|_| pr.has_next_page)
            .map(|ec| url_ctx.cursor_url("after_cursor", ec))
            .unwrap_or_default();

        (prev, next)
    } else {
        (
            url_ctx.page_url(pagination.page - 1),
            url_ctx.page_url(pagination.page + 1),
        )
    };

    PaginationContext::from_result(pr, prev_url, next_url)
}

/// The `where[…]` params of the view, prefixed with the trash flag in the
/// trash view — the view state every list link carries.
fn view_params(inputs: &ListInputs) -> String {
    let where_params = extract_where_params(&inputs.raw_query);

    if !inputs.is_trash {
        return where_params;
    }

    if where_params.is_empty() {
        return "trash=1".to_string();
    }

    format!("trash=1&{where_params}")
}

/// The search form's hidden inputs as `{name, value}` objects.
fn search_params(url_ctx: &ListUrlContext) -> Vec<Value> {
    url_ctx
        .search_form_params()
        .into_iter()
        .map(|(name, value)| json!({ "name": name, "value": value }))
        .collect()
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

/// The table-related parts of the list page.
struct ListTable {
    table_columns: Vec<Value>,
    column_options: Vec<Value>,
    filter_fields: Vec<Value>,
    active_filters: Vec<Value>,
    active_filter_count: usize,
    title_label: Option<String>,
}

/// Build the columns (the viewer's saved `user_columns` choice, when any),
/// column picker, filter fields, and filter pills.
fn build_list_table(
    args: &BuildListPageInput<'_>,
    url_ctx: &ListUrlContext,
    access: &ListFieldAccess,
    user_columns: Option<&[String]>,
) -> ListTable {
    let BuildListPageInput {
        state,
        def,
        inputs,
        auth_user,
        ..
    } = *args;

    let table_columns = resolve_columns(def, user_columns, url_ctx, access);
    let column_keys: Vec<String> = table_columns
        .iter()
        .filter_map(|c| c["key"].as_str().map(str::to_string))
        .collect();

    let active_filters = build_filter_pills(&FilterPillInputs {
        parsed: &inputs.url_filters,
        def,
        raw_query: &inputs.raw_query,
        base_url: url_ctx.base_url,
        status_filter: inputs.status_filter.as_ref(),
        translations: &state.translations,
        locale: ui_locale_of(auth_user),
    });

    ListTable {
        column_options: build_column_options(def, &column_keys, access),
        filter_fields: build_filter_fields(def, access),
        active_filter_count: active_filter_count(&active_filters, &inputs.url_filters),
        active_filters,
        title_label: title_label(def, access),
        table_columns,
    }
}

/// Inputs to [`build_list_page`]. All fields required; constructed at the
/// single call site in [`list_items`] — plain struct literal per CLAUDE.md.
struct BuildListPageInput<'a> {
    state: &'a AdminState,
    def: &'a CollectionDefinition,
    inputs: &'a ListInputs,
    claims: Option<&'a Claims>,
    auth_user: Option<&'a Extension<AuthUser>>,
}

/// Assemble the typed `CollectionItemsListPage` view-model from the
/// parsed inputs and the fetched documents.
fn build_list_page(args: &BuildListPageInput<'_>, fetched: FetchedList) -> CollectionItemsListPage {
    let BuildListPageInput {
        state,
        def,
        inputs,
        claims,
        auth_user,
    } = *args;

    let base_url = paths::collection(&def.slug);
    let where_params = view_params(inputs);
    let url_ctx = ListUrlContext {
        base_url: &base_url,
        search: inputs.search.as_deref(),
        sort: inputs.sort.as_deref(),
        per_page: inputs.per_page,
        where_params: &where_params,
    };

    let access = ListFieldAccess::new(fetched.unreadable);
    let table = build_list_table(args, &url_ctx, &access, fetched.user_columns.as_deref());
    let (title_sort_url, title_sorted_asc, title_sorted_desc) =
        compute_title_sort(def, &url_ctx, &access);

    let docs = fetched
        .result
        .docs
        .iter()
        .map(|doc| build_item_row(doc, &table.table_columns, def))
        .collect();

    let pagination = build_list_pagination(
        &fetched.result.pagination,
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

    CollectionItemsListPage {
        base,
        collection: CollectionContext::from_def(def),
        perms: CollectionPermissions::for_user(state, def, auth_user),
        docs,
        pagination,
        has_drafts: def.has_drafts(),
        has_soft_delete: def.soft_delete,
        is_trash: inputs.is_trash,
        search: inputs.search.clone(),
        sort: inputs.sort.clone(),
        search_params: search_params(&url_ctx),
        clear_search_url: url_ctx.clear_search_url(),
        is_filtered: inputs.is_filtered(),
        trash_total: fetched.trash_total,
        table_columns: table.table_columns,
        column_options: table.column_options,
        filter_fields: table.filter_fields,
        active_filters: table.active_filters,
        active_filter_count: table.active_filter_count,
        title_label: table.title_label,
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
    let hx = HxNav::from_headers(&headers);
    let def = match require_collection(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    let req = ListRequest {
        params,
        uri: &uri,
        headers: &headers,
        ui_locale: ui_locale_of(auth_user.as_ref()),
    };

    let inputs = match parse_list_inputs(&state, &def, req) {
        Ok(i) => i,
        Err(resp) => return *resp,
    };

    let fetched = match fetch_list_items(&state, def.clone(), &inputs, auth_user.clone()).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    let ctx = build_list_page(
        &BuildListPageInput {
            state: &state,
            def: &def,
            inputs: &inputs,
            claims: claims.as_ref().map(|Extension(c)| c),
            auth_user: auth_user.as_ref(),
        },
        fetched,
    );

    render_page(
        &state,
        PageRequest::new(hx, auth_user.as_ref()),
        "collections/items",
        &ctx,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::core::{FieldDefinition, FieldType};

    fn titled_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.admin.use_as_title = Some("title".to_string());
        def
    }

    fn ctx(sort: Option<&'static str>) -> ListUrlContext<'static> {
        ListUrlContext {
            base_url: "/admin/collections/posts",
            search: None,
            sort,
            per_page: None,
            where_params: "",
        }
    }

    fn open() -> ListFieldAccess {
        ListFieldAccess::default()
    }

    #[test]
    fn no_title_field_yields_no_sort() {
        let (url, asc, desc) =
            compute_title_sort(&CollectionDefinition::new("posts"), &ctx(None), &open());
        assert!(url.is_none());
        assert!(!asc && !desc);
    }

    #[test]
    fn unsorted_offers_ascending_toggle() {
        let (url, asc, desc) = compute_title_sort(&titled_def(), &ctx(None), &open());
        assert!(url.is_some());
        assert!(!asc && !desc); // not currently sorted by title
    }

    #[test]
    fn ascending_active_next_toggles_to_descending() {
        let (url, asc, desc) = compute_title_sort(&titled_def(), &ctx(Some("title")), &open());
        assert!(asc && !desc);
        // next link flips to descending.
        assert!(url.unwrap().contains("sort=-title"));
    }

    #[test]
    fn descending_active_next_toggles_back_to_ascending() {
        let (url, asc, desc) = compute_title_sort(&titled_def(), &ctx(Some("-title")), &open());
        assert!(desc && !asc);
        assert!(url.unwrap().contains("sort=title"));
    }

    /// A title field the viewer may not read gets no sort link — clicking it
    /// would be refused.
    #[test]
    fn an_unreadable_title_field_is_not_sortable() {
        let denied = ListFieldAccess::new(HashSet::from(["title".to_string()]));
        let (url, _, _) = compute_title_sort(&titled_def(), &ctx(None), &denied);
        assert!(url.is_none());
    }
}
