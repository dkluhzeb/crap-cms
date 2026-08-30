use axum::{
    Extension,
    extract::{Path, State},
    response::Response,
};
use tokio::task;

use std::sync::Arc;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{finish_version_restore, get_user_doc, paths, redirect_response},
    },
    core::{CollectionDefinition, Document, auth::AuthUser},
    service::{AppInfra, ServiceContext, ServiceError, restore_collection_version},
};

/// Owned inputs for the spawn-blocking restore body. Process-stable dependencies
/// come from the shared [`AppInfra`]; the rest is per-call.
struct RestoreVersionInput {
    infra: Arc<AppInfra>,
    slug: String,
    def: Arc<CollectionDefinition>,
    user_doc: Option<Document>,
    id: String,
    version_id: String,
}

/// Build the service context and run the version-restore service call. Wraps
/// the inline closure body so the `spawn_blocking` call is a single fn
/// invocation per CLAUDE.md.
fn restore_collection_version_blocking(
    input: &RestoreVersionInput,
) -> Result<Document, ServiceError> {
    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .infra(&input.infra)
        .user(input.user_doc.as_ref())
        .build();

    restore_collection_version(
        &ctx,
        &input.id,
        &input.version_id,
        &input.infra.locale_config,
    )
}

/// `POST /admin/collections/{slug}/{id}/versions/{version_id}/restore` — restore a version
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, id, version_id)): Path<(String, String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let Some(def) = state.infra.registry.get_collection(&slug).cloned() else {
        return redirect_response(paths::COLLECTIONS_ROOT);
    };

    if !def.has_versions() {
        return redirect_response(&paths::collection_item(&slug, &id));
    }

    let redirect = paths::collection_item(&slug, &id);
    let input = RestoreVersionInput {
        infra: state.infra.clone(),
        slug,
        def,
        user_doc: get_user_doc(auth_user.as_ref()).cloned(),
        id,
        version_id,
    };

    let result = task::spawn_blocking(move || restore_collection_version_blocking(&input)).await;

    finish_version_restore(&state, result, &redirect, "version")
}
