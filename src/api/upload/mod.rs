//! HTTP upload API: JSON endpoints for programmatic file uploads.
//!
//! Routes:
//! - `POST   /api/upload/{slug}`      — upload file + create document
//! - `PATCH  /api/upload/{slug}/{id}`  — replace file on existing document
//! - `DELETE /api/upload/{slug}/{id}`  — delete upload document + files

mod create;
mod delete;
mod helpers;
mod update;

use axum::{
    Router,
    routing::{delete as delete_route, patch, post},
};

use crate::admin::AdminState;

/// Build the upload API router with all routes.
pub fn upload_router(state: AdminState) -> Router<AdminState> {
    Router::new()
        .route("/upload/{slug}", post(create::create_upload))
        .route("/upload/{slug}/{id}", patch(update::update_upload))
        .route("/upload/{slug}/{id}", delete_route(delete::delete_upload))
        .with_state(state)
}
