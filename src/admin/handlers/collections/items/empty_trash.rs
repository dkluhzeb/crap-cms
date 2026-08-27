//! POST /admin/collections/{slug}/empty-trash — permanently delete all trashed documents.

use axum::{
    Extension,
    extract::{Path, State},
    response::{IntoResponse, Json, Response},
};
use serde_json::json;
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{
            json_bad_request, json_forbidden, json_server_error, require_collection_json,
        },
    },
    core::{AuthUser, CollectionDefinition, Document, upload},
    db::{Filter, FilterClause, FilterOp},
    service::{AppInfra, DeleteManyOptions, ServiceContext, ServiceError, delete_many},
};

/// Bundled inputs for [`empty_trash`]. Process-stable dependencies come from the
/// shared [`AppInfra`]; the rest is per-call. Grouped to keep the spawn-blocking
/// closure tidy and to satisfy the `>4 args` rule from CLAUDE.md.
struct EmptyTrashInput<'a> {
    infra: &'a AppInfra,
    def: &'a CollectionDefinition,
    slug: &'a str,
    user_doc: Option<&'a Document>,
    bulk_max_documents: i64,
}

/// Build trash filters: match only soft-deleted documents.
fn trash_filters() -> Vec<FilterClause> {
    vec![FilterClause::Single(Filter {
        field: "_deleted_at".to_string(),
        op: FilterOp::Exists,
    })]
}

/// Find all trashed documents and permanently delete them via the service layer.
fn empty_trash(input: &EmptyTrashInput<'_>) -> Result<usize, ServiceError> {
    // Clone the def with `soft_delete = false` so the per-row delete hard-deletes
    // (gated by `access.delete`) instead of re-trashing. Note this also makes the
    // delete's `enforce_access_constraints` count the trashed rows: the read
    // count only appends `_deleted_at IS NULL` when `def.soft_delete` is true, so
    // with the flag cleared a `Constrained` delete rule still matches the (now
    // physically-trashed) rows. Keep both behaviors tied to this single flag.
    let mut hard_def = input.def.clone();
    hard_def.make_hard_delete();

    let filters = trash_filters();

    let ctx = ServiceContext::collection(input.slug, &hard_def)
        .infra(input.infra)
        .user(input.user_doc)
        // Bulk trash purge is quiet — no per-document live-update events.
        .emit_events(false)
        .build();

    let delete_opts = DeleteManyOptions {
        run_hooks: true,
        include_deleted: true,
        max_documents: input.bulk_max_documents,
    };

    let result = delete_many(&ctx, &filters, &input.infra.locale_config, &delete_opts)?;

    for fields in &result.upload_fields_to_clean {
        upload::delete_upload_files(input.infra.storage.as_ref(), fields);
    }

    Ok(usize::try_from(result.hard_deleted.max(0)).unwrap_or(0))
}

/// POST /admin/collections/{slug}/empty-trash
#[cfg(not(tarpaulin_include))]
pub async fn empty_trash_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match require_collection_json(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    if !def.soft_delete {
        return json_bad_request("Collection does not support soft delete");
    }

    let infra = state.infra.clone();
    let bulk_max_documents = state.config.server.bulk_max_documents;
    let user_doc = auth_user.as_ref().map(|Extension(au)| au.user_doc.clone());

    let result = task::spawn_blocking(move || {
        empty_trash(&EmptyTrashInput {
            infra: &infra,
            def: &def,
            slug: &slug,
            user_doc: user_doc.as_ref(),
            bulk_max_documents,
        })
    })
    .await;

    match result {
        Ok(Ok(count)) => Json(json!({"ok": true, "count": count})).into_response(),
        Ok(Err(ServiceError::AccessDenied(_))) => {
            json_forbidden("You don't have permission to empty the trash")
        }
        Ok(Err(e)) => {
            error!("Empty trash error: {}", e);
            json_server_error("Failed to empty trash")
        }
        Err(e) => {
            error!("Empty trash task error: {}", e);
            json_server_error("Internal error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_filters_select_only_soft_deleted_rows() {
        let filters = trash_filters();
        assert_eq!(filters.len(), 1);
        assert!(matches!(
            &filters[0],
            FilterClause::Single(f) if f.field == "_deleted_at" && matches!(f.op, FilterOp::Exists)
        ));
    }
}
