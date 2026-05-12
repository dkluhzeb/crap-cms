use axum::{
    Extension,
    extract::{Path, State},
    response::Response,
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{get_user_doc, htmx_redirect, paths, redirect_response},
    },
    config::LocaleConfig,
    core::{AuthUser, Document, GlobalDefinition, SharedCache, SharedEventTransport},
    db::DbPool,
    hooks::HookRunner,
    service::{ServiceContext, ServiceError, restore_global_version},
};

/// Owned inputs for the spawn-blocking restore body.
struct RestoreGlobalVersionInput {
    pool: DbPool,
    runner: HookRunner,
    slug: String,
    def: GlobalDefinition,
    user_doc: Option<Document>,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    version_id: String,
    locale_config: LocaleConfig,
}

/// Build the service context and run the global version-restore service call.
fn restore_global_version_blocking(
    input: RestoreGlobalVersionInput,
) -> Result<Document, ServiceError> {
    let ctx = ServiceContext::global(&input.slug, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(input.user_doc.as_ref())
        .event_transport(input.event_transport)
        .cache(input.cache)
        .build();

    restore_global_version(&ctx, &input.version_id, &input.locale_config)
}

/// POST /admin/globals/{slug}/versions/{version_id}/restore
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, version_id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let Some(def) = state.registry.get_global(&slug).cloned() else {
        return redirect_response(paths::DASHBOARD);
    };

    if !def.has_versions() {
        return redirect_response(&paths::global(&slug));
    }

    let redirect = paths::global(&slug);
    let input = RestoreGlobalVersionInput {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        slug,
        def,
        user_doc: get_user_doc(&auth_user).cloned(),
        event_transport: state.event_transport.clone(),
        cache: state.cache.clone(),
        version_id,
        locale_config: state.config.locale.clone(),
    };

    let result = task::spawn_blocking(move || restore_global_version_blocking(input)).await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&redirect),
        Ok(Err(e)) => {
            error!("Restore global version error: {}", e);
            htmx_redirect(&redirect)
        }
        Err(e) => {
            error!("Restore global version task error: {}", e);
            htmx_redirect(&redirect)
        }
    }
}
