//! Axum router declaration for the upload API.

use axum::{
    Router,
    routing::{delete as delete_route, patch, post},
};

use crate::admin::AdminState;

use super::{create, delete, update};

/// Build the upload API router with all routes.
pub fn upload_router(state: AdminState) -> Router<AdminState> {
    Router::new()
        .route("/upload/{slug}", post(create::create_upload))
        .route("/upload/{slug}/{id}", patch(update::update_upload))
        .route("/upload/{slug}/{id}", delete_route(delete::delete_upload))
        .with_state(state)
}
