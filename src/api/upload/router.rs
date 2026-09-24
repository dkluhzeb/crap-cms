//! Axum router declaration for the upload API.

use axum::{
    Router, middleware,
    routing::{delete as delete_route, patch, post},
};

use crate::admin::{AdminState, upload_body_limit};

use super::{create, delete, update};

/// Build the upload API router with all routes.
///
/// Create and update carry a file, so their body limit follows the target
/// collection's upload maximum rather than the global default.
pub fn upload_router(state: AdminState) -> Router<AdminState> {
    let upload_limit = || middleware::from_fn_with_state(state.clone(), upload_body_limit);

    Router::new()
        .route(
            "/upload/{slug}",
            post(create::create_upload).route_layer(upload_limit()),
        )
        .route(
            "/upload/{slug}/{id}",
            patch(update::update_upload).route_layer(upload_limit()),
        )
        .route("/upload/{slug}/{id}", delete_route(delete::delete_upload))
        .with_state(state)
}
