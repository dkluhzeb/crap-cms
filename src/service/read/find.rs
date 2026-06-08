//! Paginated find query with the full read lifecycle.

use crate::{
    core::{CollectionDefinition, Document},
    db::{AccessResult, Filter, FilterClause, FilterOp, FindQuery, LocaleContext, query},
    hooks::AccessCheckInput,
    service::{FindDocumentsInput, PaginatedResult, ServiceContext, ServiceError, helpers},
};

use super::draft_visibility::draft_visibility_filter;
use super::post_process::post_process_docs;
use super::validate_filters::{validate_access_constraints, validate_user_filters};

type Result<T> = std::result::Result<T, ServiceError>;

/// Execute a paginated find query with the full read lifecycle.
///
/// Steps: validate user filters -> access check -> inject system filters ->
/// `before_read` -> find + count -> post-process -> build pagination.
/// Returns `PaginatedResult<Document>` with docs, total, and computed pagination metadata.
///
/// # Errors
///
/// Returns service-layer errors (access denied, invalid filter, hook errors)
/// or a backend error if the find/count queries fail.
pub fn find_documents(
    ctx: &ServiceContext,
    input: &FindDocumentsInput,
) -> Result<PaginatedResult<Document>> {
    validate_user_filters(&input.query.filters)?;

    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.collection_def()?;

    let access_ref = if input.trash {
        def.access.resolve_trash()
    } else {
        def.access.read.as_deref()
    };

    let access = hooks.check_access(&AccessCheckInput {
        access_ref,
        user: ctx.user,
        id: None,
        data: None,
        locale: input.locale_ctx.map(LocaleContext::access_locale),
        operation: "find",
        collection: ctx.slug,
        ui_locale: None,
    })?;

    if matches!(access, AccessResult::Denied) {
        let msg = if input.trash {
            "Trash access denied"
        } else {
            "Read access denied"
        };
        return Err(ServiceError::AccessDenied(msg.into()));
    }

    let mut fq = build_effective_query(
        input.query,
        def,
        input.trash,
        input.include_drafts,
        input.status_filter.as_deref(),
    );

    // `_status` filtering is "happening" if the service-layer injected
    // either the published-only default OR an explicit user-supplied
    // status. Access-constraint hooks that mention `_status` are
    // permitted in either case (the SQL `_status` column is being
    // queried regardless of which value).
    let injecting_status = (input.status_filter.is_some()
        || (!input.include_drafts && def.has_drafts()))
        && def.has_drafts();

    if let AccessResult::Constrained(extra) = access {
        validate_access_constraints(&extra, input.trash, injecting_status, ctx.slug)?;
        fq.filters.extend(extra);
    }

    hooks.before_read(
        &def.hooks,
        ctx.slug,
        "find",
        input.locale_ctx.map(LocaleContext::access_locale),
    )?;

    let had_cursor = fq.after_cursor.is_some() || fq.before_cursor.is_some();
    let overfetch = input.cursor_enabled && had_cursor;

    if overfetch {
        fq.limit = fq.limit.map(|l| l + 1);
    }

    let mut docs = query::find(conn, ctx.slug, def, &fq, input.locale_ctx)?;

    let total = query::count_with_search(
        conn,
        ctx.slug,
        def,
        &fq.filters,
        input.locale_ctx,
        fq.search.as_deref(),
        fq.include_deleted,
    )?;

    // Restore original limit for pagination calculation.
    if overfetch {
        fq.limit = fq.limit.map(|l| l - 1);
    }

    let limit = fq.limit.unwrap_or(total);

    // Detect whether more pages exist via overfetch, then trim the extra doc.
    // Saturate doc count for the unreachable case so the > check still works.
    let docs_count = i64::try_from(docs.len()).unwrap_or(i64::MAX);
    let cursor_has_more = if overfetch {
        if docs_count > limit {
            if fq.before_cursor.is_some() {
                docs.remove(0);
            } else {
                docs.pop();
            }
            Some(true)
        } else {
            Some(false)
        }
    } else {
        None
    };

    post_process_docs(ctx, conn, &mut docs, input);

    let pagination = helpers::build_pagination(&helpers::PaginationInputs {
        docs: &docs,
        total,
        fq: &fq,
        cursor_enabled: input.cursor_enabled,
        has_timestamps: def.timestamps,
        has_drafts: def.has_drafts(),
        had_cursor,
        cursor_has_more,
    });

    Ok(PaginatedResult {
        docs,
        total,
        pagination,
    })
}

/// Clone the user-supplied query and inject service-owned system filters
/// (`_status` and `_deleted_at`) based on the typed flags.
///
/// Runs *after* `validate_user_filters` so the injected filters bypass the
/// system-column rule that user filters are subject to.
///
/// Status precedence: an explicit `status_filter` (translated by the admin
/// list handler from `?where[_status][equals]=X` URL params, including
/// OR-bucket forms) wins. One value injects `_status = X`; multiple
/// values widen to `_status IN (X, Y, …)`. Otherwise the
/// default-when-drafts rule fires: `include_drafts = false` &&
/// `def.has_drafts()` injects `_status = "published"`.
fn build_effective_query(
    user_query: &FindQuery,
    def: &CollectionDefinition,
    trash: bool,
    include_drafts: bool,
    status_filter: Option<&[String]>,
) -> FindQuery {
    let mut fq = user_query.clone();

    match status_filter {
        Some(values) if def.has_drafts() && !values.is_empty() => {
            let op = if values.len() == 1 {
                FilterOp::Equals(values[0].clone())
            } else {
                FilterOp::In(values.to_vec())
            };
            fq.filters.push(FilterClause::Single(Filter {
                field: "_status".to_string(),
                op,
            }));
        }
        // No explicit status filter → apply the shared default-draft rule.
        _ => {
            if let Some(f) = draft_visibility_filter(def, include_drafts) {
                fq.filters.push(f);
            }
        }
    }

    if trash && def.soft_delete {
        fq.include_deleted = true;
        fq.filters.push(FilterClause::Single(Filter {
            field: "_deleted_at".to_string(),
            op: FilterOp::Exists,
        }));
    }

    fq
}

#[cfg(test)]
mod tests {
    use crate::core::CollectionDefinition;
    use crate::core::collection::VersionsConfig;

    use super::*;

    fn drafts_collection() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.versions = Some(VersionsConfig::new(true, 5));
        def
    }

    /// Extract the `_status` filter op, if any was injected.
    fn status_op(fq: &FindQuery) -> Option<&FilterOp> {
        fq.filters.iter().find_map(|c| match c {
            FilterClause::Single(f) if f.field == "_status" => Some(&f.op),
            _ => None,
        })
    }

    /// SECURITY-CRITICAL: a drafts collection with `include_drafts = false`
    /// and no explicit status filter must inject `_status = "published"`, so
    /// unpublished drafts never leak to a default read.
    #[test]
    fn drafts_hidden_by_default_inject_published_status() {
        let fq = build_effective_query(
            &FindQuery::default(),
            &drafts_collection(),
            false,
            false,
            None,
        );
        assert!(
            matches!(status_op(&fq), Some(FilterOp::Equals(v)) if v == "published"),
            "expected _status = published, got {:?}",
            status_op(&fq)
        );
    }

    #[test]
    fn include_drafts_true_adds_no_status_filter() {
        let fq = build_effective_query(
            &FindQuery::default(),
            &drafts_collection(),
            false,
            true,
            None,
        );
        assert!(status_op(&fq).is_none());
    }

    #[test]
    fn non_drafts_collection_never_filters_status() {
        let def = CollectionDefinition::new("posts"); // no versions → no drafts
        let fq = build_effective_query(&FindQuery::default(), &def, false, false, None);
        assert!(status_op(&fq).is_none());
    }

    #[test]
    fn explicit_status_filter_uses_equals_for_one_in_for_many() {
        let one = build_effective_query(
            &FindQuery::default(),
            &drafts_collection(),
            false,
            false,
            Some(&["draft".to_string()]),
        );
        assert!(matches!(status_op(&one), Some(FilterOp::Equals(v)) if v == "draft"));

        let many = build_effective_query(
            &FindQuery::default(),
            &drafts_collection(),
            false,
            false,
            Some(&["draft".to_string(), "published".to_string()]),
        );
        assert!(matches!(status_op(&many), Some(FilterOp::In(v)) if v.len() == 2));
    }

    #[test]
    fn trash_includes_deleted_and_requires_deleted_at() {
        let mut def = drafts_collection();
        def.soft_delete = true;
        let fq = build_effective_query(&FindQuery::default(), &def, true, true, None);

        assert!(fq.include_deleted);
        assert!(fq.filters.iter().any(|c| matches!(
            c,
            FilterClause::Single(f) if f.field == "_deleted_at" && matches!(f.op, FilterOp::Exists)
        )));
    }
}
