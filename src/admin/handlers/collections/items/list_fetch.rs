//! The blocking read behind the collection list page: which fields the viewer
//! may query, the trash total, their saved columns, and the rows themselves.

use std::{collections::HashSet, sync::Arc};

use axum::{Extension, response::Response};

use crate::{
    admin::{
        AdminState,
        handlers::shared::{
            StatusFilter, forbidden, is_column_eligible, service_error_to_admin_response,
            task_join_error_response,
        },
    },
    core::{AuthUser, CollectionDefinition, Document, spawn_blocking_in_label_locale},
    db::{
        DbConnection,
        query::{FindQuery, LocaleContext},
    },
    service::{
        CountDocumentsInput, PaginatedResult, QueryFieldRefs, RunnerReadHooks, ServiceContext,
        ServiceError, count_documents,
        op::{self, CoreError, Find, FindArgs, Principal, TargetRef},
        query_field_paths, unreadable_query_paths,
        user_settings::load_user_settings,
    },
};

use super::list_inputs::ListInputs;

/// Owned inputs for the blocking list read. Constructed at the single call
/// site in [`fetch_list_items`] — plain struct literal per CLAUDE.md's
/// "single call site" exception to the builder rule.
struct FetchListArgs {
    state: AdminState,
    def: Arc<CollectionDefinition>,
    find_query: FindQuery,
    locale_ctx: Option<LocaleContext>,
    auth_user: Option<Extension<AuthUser>>,
    cursor_enabled: bool,
    is_trash: bool,
    status_filter: Option<Vec<String>>,
    /// Whether `find_query.order_by` is the viewer's own `?sort=` (as opposed
    /// to the collection's `admin.default_sort`).
    user_sorted: bool,
}

/// What the list page shows beyond the rows themselves.
pub(super) struct FetchedList {
    pub result: PaginatedResult<Document>,
    /// Top-level fields the viewer may not sort, filter, or show as a column.
    pub unreadable: HashSet<String>,
    /// Trash view only: the unfiltered trash count.
    pub trash_total: Option<i64>,
    /// The viewer's saved column choice for this collection, if any.
    pub user_columns: Option<Vec<String>>,
}

/// What the read learns before the find: the fields the viewer may not
/// query, the trash total, and the viewer's saved columns.
struct ListProbe {
    unreadable: HashSet<String>,
    trash_total: Option<i64>,
    user_columns: Option<Vec<String>>,
}

/// Why the list read failed.
enum ListFetchError {
    Service(ServiceError),
    /// The viewer's own sort or filter names a field they may not read.
    UnreadableField(String),
}

impl From<ServiceError> for ListFetchError {
    fn from(e: ServiceError) -> Self {
        Self::Service(e)
    }
}

/// Every top-level list-eligible field, plus every path the query references
/// — the candidates the readability probe answers for.
fn probe_paths(def: &CollectionDefinition, find_query: &FindQuery) -> Vec<String> {
    let refs = QueryFieldRefs {
        filters: &find_query.filters,
        order_by: find_query.order_by.as_deref(),
    };

    let mut paths: Vec<String> = def
        .fields
        .iter()
        .filter(|f| is_column_eligible(&f.field_type))
        .map(|f| f.name.clone())
        .collect();

    for path in query_field_paths(&refs) {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }

    paths
}

/// Refuse a query whose own sort or filters name an unreadable field, and
/// drop a default sort the viewer may not use (the list then falls back to
/// the built-in order instead of refusing the whole collection).
/// `user_sorted` says whether `order_by` is the viewer's own `?sort=`.
fn settle_query(
    find_query: &mut FindQuery,
    user_sorted: bool,
    unreadable: &HashSet<String>,
) -> Result<(), ListFetchError> {
    let user_sort = find_query.order_by.as_deref().filter(|_| user_sorted);
    let own = query_field_paths(&QueryFieldRefs {
        filters: &find_query.filters,
        order_by: user_sort,
    });

    if let Some(path) = own.into_iter().find(|p| unreadable.contains(p)) {
        return Err(ListFetchError::UnreadableField(path));
    }

    let default_sort_unreadable = find_query
        .order_by
        .as_deref()
        .is_some_and(|o| unreadable.contains(o.strip_prefix('-').unwrap_or(o)));
    if !user_sorted && default_sort_unreadable {
        find_query.order_by = None;
    }

    Ok(())
}

/// The fields among [`probe_paths`] the viewer may not query.
fn unreadable_fields(
    ctx: &ServiceContext<'_>,
    args: &FetchListArgs,
) -> Result<HashSet<String>, ServiceError> {
    let locale = args.locale_ctx.as_ref().map(LocaleContext::access_locale);
    let paths = probe_paths(&args.def, &args.find_query);

    Ok(unreadable_query_paths(ctx, locale, &paths)?
        .into_iter()
        .collect())
}

/// Trash view only: how many documents the whole trash holds.
fn trash_total(
    ctx: &ServiceContext<'_>,
    args: &FetchListArgs,
) -> Result<Option<i64>, ServiceError> {
    if !args.is_trash {
        return Ok(None);
    }

    let input = CountDocumentsInput::builder(&[])
        .locale_ctx(args.locale_ctx.as_ref())
        .include_drafts(true)
        .trash(true)
        .build();

    Ok(Some(count_documents(ctx, &input)?))
}

/// The viewer's saved column preferences for the collection. Unreadable
/// settings fall back to the default columns rather than failing the page.
fn user_columns(conn: &dyn DbConnection, args: &FetchListArgs) -> Option<Vec<String>> {
    let Extension(au) = args.auth_user.as_ref()?;

    load_user_settings(conn, &au.claims.sub)
        .ok()?
        .columns(&args.def.slug)
}

/// Probe which fields the viewer may not query, count the whole trash (trash
/// view), and load the viewer's saved columns — through the same read hooks
/// and connection the find's checks use.
fn probe_list(
    args: &FetchListArgs,
    user: Option<&Document>,
    ui_locale: Option<&str>,
) -> Result<ListProbe, ServiceError> {
    let conn = args
        .state
        .infra
        .pool
        .get()
        .map_err(ServiceError::Internal)?;
    let hooks = RunnerReadHooks::new(&args.state.infra.hook_runner, &conn, user, ui_locale);
    let ctx = ServiceContext::collection(&args.def.slug, &args.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user)
        .ui_locale(ui_locale.map(str::to_string))
        .build();

    Ok(ListProbe {
        unreadable: unreadable_fields(&ctx, args)?,
        trash_total: trash_total(&ctx, args)?,
        user_columns: user_columns(&conn, args),
    })
}

/// The blocking list read: probe field access, settle the query against it,
/// then run the shared service `Find`.
///
/// `is_trash` is a presentation flag from the request — the service layer
/// injects `_deleted_at EXISTS` and flips `include_deleted` itself. The admin
/// list view shows drafts alongside published rows for users who can view them
/// (edit-level access); a read-only viewer sees published only.
fn fetch_list_documents(mut args: FetchListArgs) -> Result<FetchedList, ListFetchError> {
    let ui_locale = args
        .auth_user
        .as_ref()
        .map(|Extension(au)| au.ui_locale.clone());
    let user_doc = args
        .auth_user
        .as_ref()
        .map(|Extension(au)| au.user_doc.clone());

    let probe = probe_list(&args, user_doc.as_ref(), ui_locale.as_deref())?;
    settle_query(&mut args.find_query, args.user_sorted, &probe.unreadable)?;

    // Request drafts unconditionally — the service read path returns the union
    // of the views the caller may see and downgrades (never rejects): an editor
    // gets published + drafts, a read-only admin gets published only. Draft
    // *visibility* is the service's job; `CollectionPermissions` survives only
    // as a UI hint (show/hide the Drafts tab), not a request gate.
    let op_args = FindArgs::builder(args.find_query)
        .hydrate(false)
        .locale_ctx(args.locale_ctx)
        .cursor_enabled(args.cursor_enabled)
        .trash(args.is_trash)
        .include_drafts(true)
        .status_filter(args.status_filter)
        .build();

    let result = op::run::<Find>(
        &args.state.infra,
        Principal::Resolved {
            user: user_doc,
            ui_locale,
        },
        &TargetRef::collection(&*args.def.slug),
        op_args,
    )
    .map_err(CoreError::into_service_error)?;

    Ok(FetchedList {
        result,
        unreadable: probe.unreadable,
        trash_total: probe.trash_total,
        user_columns: probe.user_columns,
    })
}

/// Map a failed list read to the admin page it renders.
fn list_error_response(state: &AdminState, error: ListFetchError, is_trash: bool) -> Response {
    match error {
        ListFetchError::UnreadableField(path) => forbidden(
            state,
            &format!("You can't sort or filter this list by '{path}': the field is not readable"),
        ),
        ListFetchError::Service(e) => {
            let denied_msg = if is_trash {
                "You don't have permission to view the trash"
            } else {
                "You don't have permission to view this collection"
            };

            service_error_to_admin_response(state, e, denied_msg)
        }
    }
}

/// Run the list read on the blocking pool (in the request's label locale) and
/// map the service error / join error to an admin-rendered response.
pub(super) async fn fetch_list_items(
    state: &AdminState,
    def: Arc<CollectionDefinition>,
    inputs: &ListInputs,
    auth_user: Option<Extension<AuthUser>>,
) -> Result<FetchedList, Response> {
    let args = FetchListArgs {
        state: state.clone(),
        def,
        find_query: inputs.find_query.clone(),
        locale_ctx: inputs.locale_ctx.clone(),
        auth_user,
        cursor_enabled: inputs.cursor_enabled,
        is_trash: inputs.is_trash,
        status_filter: inputs.status_filter.as_ref().map(StatusFilter::requested),
        user_sorted: inputs.sort.is_some(),
    };

    match spawn_blocking_in_label_locale(move || fetch_list_documents(args)).await {
        Ok(Ok(fetched)) => Ok(fetched),
        Ok(Err(e)) => Err(list_error_response(state, e, inputs.is_trash)),
        Err(e) => Err(task_join_error_response(state, &e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{FieldDefinition, FieldType},
        db::query::{Filter, FilterClause, FilterOp},
    };

    fn query_with(order_by: Option<&str>, filters: Vec<FilterClause>) -> FindQuery {
        FindQuery::builder()
            .filters(filters)
            .order_by(order_by.map(str::to_string))
            .build()
    }

    fn unreadable(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    fn filter(field: &str) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.to_string(),
            op: FilterOp::Equals("x".into()),
        })
    }

    /// Run `settle_query` and return the order it settles on.
    fn settle_order(
        order_by: Option<&str>,
        filters: Vec<FilterClause>,
        user_sorted: bool,
        unreadable: &HashSet<String>,
    ) -> Result<Option<String>, ListFetchError> {
        let mut find_query = query_with(order_by, filters);

        settle_query(&mut find_query, user_sorted, unreadable)?;

        Ok(find_query.order_by)
    }

    #[test]
    fn probe_paths_cover_eligible_fields_and_query_references() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("body", FieldType::Richtext).build(),
        ];
        let query = query_with(Some("-seo__title"), vec![filter("title")]);

        assert_eq!(probe_paths(&def, &query), vec!["title", "seo__title"]);
    }

    /// The viewer's own sort or filter on a field they may not read is refused
    /// with a message naming the field — not the whole-collection 403.
    #[test]
    fn user_sort_or_filter_on_an_unreadable_field_is_refused() {
        let hidden = unreadable(&["secret"]);

        let sorted = settle_order(Some("-secret"), vec![], true, &hidden);
        assert!(matches!(sorted, Err(ListFetchError::UnreadableField(p)) if p == "secret"));

        let filtered = settle_order(None, vec![filter("secret")], true, &hidden);
        assert!(matches!(filtered, Err(ListFetchError::UnreadableField(p)) if p == "secret"));
    }

    /// Regression: an `admin.default_sort` the viewer may not read turned the
    /// whole list into a 403. It is dropped for that viewer instead.
    #[test]
    fn an_unreadable_default_sort_is_dropped() {
        let hidden = unreadable(&["secret"]);

        let settled = settle_order(Some("-secret"), vec![], false, &hidden);
        assert!(matches!(settled, Ok(None)));

        let kept = settle_order(Some("-title"), vec![], false, &hidden);
        assert!(matches!(kept, Ok(Some(o)) if o == "-title"));
    }
}
