//! Request parsing behind the collection list page: the URL's pagination,
//! sort, filters, search and editor locale as one validated bundle.

use axum::{
    http::{HeaderMap, Uri},
    response::Response,
};
use tracing::warn;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{
            PaginationParams, StatusFilter, bad_request, editor_locale_ctx, extract_editor_locale,
            extract_status_filter, parse_where_params, validate_sort,
        },
    },
    core::CollectionDefinition,
    db::query::{self, FilterClause, FindQuery, LocaleContext},
};

/// Validated + normalized inputs derived from the request URL/headers,
/// produced by [`parse_list_inputs`]. All downstream phases (fetch +
/// view-model build + page-context build) take this as a single
/// reference — keeps the handler a thin orchestrator.
pub(super) struct ListInputs {
    pub is_trash: bool,
    pub cursor_enabled: bool,
    pub search: Option<String>,
    pub pagination: query::FindPagination,
    /// The page size the viewer asked for (validated), carried in every list
    /// link; `None` when the configured default applies.
    pub per_page: Option<i64>,
    /// The sort the viewer chose (`?sort=`), as opposed to the collection's
    /// `admin.default_sort`.
    pub sort: Option<String>,
    pub url_filters: Vec<FilterClause>,
    pub status_filter: Option<StatusFilter>,
    pub find_query: FindQuery,
    pub editor_locale: Option<String>,
    pub locale_ctx: Option<LocaleContext>,
    pub raw_query: String,
}

impl ListInputs {
    /// Whether a search or filter narrows the list.
    pub fn is_filtered(&self) -> bool {
        self.search.is_some() || !self.url_filters.is_empty() || self.status_filter.is_some()
    }
}

/// The parts of a list request [`parse_list_inputs`] reads. All fields are
/// required and it is built at the single call site in the list handler.
pub(super) struct ListRequest<'a> {
    pub params: PaginationParams,
    pub uri: &'a Uri,
    pub headers: &'a HeaderMap,
    /// The viewer's UI language, for translated 400 messages.
    pub ui_locale: &'a str,
}

/// The user filters the read runs: the URL's `where[…]` clauses, plus a clause
/// that matches no row when the `_status` rows contradict each other — the
/// typed status filter then requests every named status, so the viewer's views
/// still resolve, and this clause empties the result.
fn query_filters(
    url_filters: &[FilterClause],
    status_filter: Option<&StatusFilter>,
) -> Vec<FilterClause> {
    let mut filters = url_filters.to_vec();

    if status_filter.is_some_and(StatusFilter::matches_nothing) {
        filters.push(FilterClause::Or(Vec::new()));
    }

    filters
}

/// Build the `FindQuery` from pagination, user filters, sort, and search params.
///
/// Produces a *user* query — system filters (`_deleted_at`, `_status`) are
/// injected by `service::find_documents` based on the typed flags. The trash
/// default sort (`-_deleted_at`) is set by the caller as a presentation choice.
fn build_find_query(
    pagination: &query::FindPagination,
    filters: Vec<FilterClause>,
    order_by: Option<String>,
    search: Option<&str>,
) -> FindQuery {
    let offset = (!pagination.has_cursor()).then_some(pagination.offset);

    FindQuery::builder()
        .filters(filters)
        .order_by(order_by)
        .limit(Some(pagination.limit))
        .offset(offset)
        .after_cursor(pagination.after_cursor.clone())
        .before_cursor(pagination.before_cursor.clone())
        .search(search.map(str::to_string))
        .build()
}

/// Parse the typed `_status` filter. Present-but-invalid is a 400.
fn parse_status_filter(
    state: &AdminState,
    def: &CollectionDefinition,
    raw_query: &str,
) -> Result<Option<StatusFilter>, Box<Response>> {
    // The filter UI exposes `_status` for collections with drafts (see
    // `build_filter_fields`); the URL it produces (`where[_status][equals]=X`,
    // including OR-bucket forms) is handled here as a typed param rather
    // than a generic where clause, because system columns (`_*`) are
    // off-limits to user filters at the service layer
    // (`validate_user_filters`). See `extract_status_filter` for the
    // parsing rule. The statuses it admits become the requested views.
    let Some(status_filter) = extract_status_filter(raw_query) else {
        return Ok(None);
    };

    if !def.has_drafts() {
        return Err(Box::new(bad_request(
            state,
            "Status filter is not available on this collection (drafts are disabled)",
        )));
    }

    let named = status_filter.named();

    if let Some(bad) = named.iter().find(|s| *s != "draft" && *s != "published") {
        return Err(Box::new(bad_request(
            state,
            &format!("Unknown status filter value '{bad}' (valid: draft, published)"),
        )));
    }

    Ok(Some(status_filter))
}

/// Parse the generic `where[…]` filters. Present-but-invalid is a 400,
/// its message in the viewer's UI language where one is translated.
fn parse_filters(
    state: &AdminState,
    def: &CollectionDefinition,
    raw_query: &str,
    ui_locale: &str,
) -> Result<Vec<FilterClause>, Box<Response>> {
    parse_where_params(raw_query, def).map_err(|e| {
        Box::new(bad_request(
            state,
            &e.message(&state.translations, ui_locale),
        ))
    })
}

/// Validate the `?sort=` param. Present-but-invalid is a 400.
fn parse_sort(
    state: &AdminState,
    def: &CollectionDefinition,
    sort: Option<&str>,
) -> Result<Option<String>, Box<Response>> {
    let Some(s) = sort else {
        return Ok(None);
    };

    validate_sort(s, def).map(Some).ok_or_else(|| {
        Box::new(bad_request(
            state,
            &format!("Unknown or unsortable sort field '{s}'"),
        ))
    })
}

/// Parse the query string + headers + pagination params into a typed
/// [`ListInputs`] bundle. Returns a response (not an error) for the
/// invalid-pagination case so the handler can short-circuit cleanly.
pub(super) fn parse_list_inputs(
    state: &AdminState,
    def: &CollectionDefinition,
    req: ListRequest<'_>,
) -> Result<ListInputs, Box<Response>> {
    let ListRequest {
        params,
        uri,
        headers,
        ui_locale,
    } = req;

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
    let per_page = params.per_page.map(|_| pagination.limit);

    // Present-but-invalid URL params hard-error with 400 (parity with the
    // MCP/gRPC surfaces) instead of silently rendering wrong/unfiltered or
    // default-sorted results.
    let sort = parse_sort(state, def, params.sort.as_deref())?;

    let url_filters = parse_filters(state, def, &raw_query, ui_locale)?;

    let status_filter = parse_status_filter(state, def, &raw_query)?;

    // Trash view: an explicit user sort wins; with none, the shared `Find`
    // body applies the newest-deleted-first trash default. Hard-coding the
    // default here used to discard the user's column sort entirely.
    let order_by = if is_trash {
        sort.clone()
    } else {
        sort.clone().or_else(|| def.admin.default_sort.clone())
    };

    let filters = query_filters(&url_filters, status_filter.as_ref());
    let find_query = build_find_query(&pagination, filters, order_by, search.as_deref());

    let editor_locale = extract_editor_locale(headers, &state.config.locale);
    let locale_ctx = editor_locale_ctx(&state.config.locale, editor_locale.as_deref());

    Ok(ListInputs {
        is_trash,
        cursor_enabled,
        search,
        pagination,
        per_page,
        sort,
        url_filters,
        status_filter,
        find_query,
        editor_locale,
        locale_ctx,
        raw_query,
    })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use axum::http::StatusCode;
    use serde_json::{Value, from_str};

    use super::*;
    use crate::{
        admin::test_state::test_admin_state,
        core::{FieldType, field::FieldDefinition},
    };

    fn posts_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def
    }

    /// The error-toast message of a response.
    fn toast_message(resp: &Response) -> String {
        let header = resp.headers()["X-Crap-Toast"].to_str().unwrap();
        let toast: Value = from_str(header).unwrap();

        toast["message"].as_str().unwrap().to_string()
    }

    /// Regression: `_status` in a mixed OR group answered with a fixed English
    /// message. It renders in the viewer's UI language now.
    #[test]
    fn status_in_mixed_or_is_a_translated_400() {
        let state = test_admin_state();
        let query = "where[or][0][0][_status][equals]=draft&where[or][0][1][title][equals]=B";

        let resp = parse_filters(&state, &posts_def(), query, "de").unwrap_err();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            toast_message(&resp),
            state.translations.get("de", "filter_status_or_mixed")
        );
    }

    /// Other invalid filters keep their diagnostic message.
    #[test]
    fn other_invalid_filters_keep_their_message() {
        let state = test_admin_state();

        let resp = parse_filters(&state, &posts_def(), "where[nope][equals]=x", "de").unwrap_err();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(toast_message(&resp).contains("Unknown filter field 'nope'"));
    }

    /// Contradicting `_status` rows add a clause that matches no row; any other
    /// status filter leaves the URL's filters as they are.
    #[test]
    fn contradicting_status_rows_add_a_match_nothing_clause() {
        let contradiction =
            extract_status_filter("where[_status][equals]=draft&where[_status][equals]=published");
        let draft = extract_status_filter("where[_status][equals]=draft");

        let filters = query_filters(&[], contradiction.as_ref());
        assert!(
            matches!(filters.as_slice(), [FilterClause::Or(subs)] if subs.is_empty()),
            "{filters:?}"
        );
        assert!(query_filters(&[], draft.as_ref()).is_empty());
        assert!(query_filters(&[], None).is_empty());
    }
}
