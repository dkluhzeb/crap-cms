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
    core::{AuthUser, Document, GlobalDefinition},
    service::{AppInfra, ServiceContext, ServiceError, restore_global_version},
};

/// Owned inputs for the spawn-blocking restore body. Process-stable dependencies
/// come from the shared [`AppInfra`]; the rest is per-call.
struct RestoreGlobalVersionInput {
    infra: Arc<AppInfra>,
    slug: String,
    def: GlobalDefinition,
    user_doc: Option<Document>,
    version_id: String,
}

/// Build the service context and run the global version-restore service call.
fn restore_global_version_blocking(
    input: &RestoreGlobalVersionInput,
) -> Result<Document, ServiceError> {
    let ctx = ServiceContext::global(&input.slug, &input.def)
        .infra(&input.infra)
        .user(input.user_doc.as_ref())
        .build();

    restore_global_version(&ctx, &input.version_id, &input.infra.locale_config)
}

/// `POST /admin/globals/{slug}/versions/{version_id}/restore`
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, version_id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let Some(def) = state.infra.registry.get_global(&slug).cloned() else {
        return redirect_response(paths::DASHBOARD);
    };

    if !def.has_versions() {
        return redirect_response(&paths::global(&slug));
    }

    let redirect = paths::global(&slug);
    let input = RestoreGlobalVersionInput {
        infra: state.infra.clone(),
        slug,
        def,
        user_doc: get_user_doc(auth_user.as_ref()).cloned(),
        version_id,
    };

    let result = task::spawn_blocking(move || restore_global_version_blocking(&input)).await;

    finish_version_restore(&state, result, &redirect, "global version")
}
