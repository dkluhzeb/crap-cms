//! HTTP upload API: JSON endpoints for programmatic file uploads.
//!
//! Routes:
//! - `POST   /api/upload/{slug}`      — upload file + create document
//! - `PATCH  /api/upload/{slug}/{id}`  — replace file on existing document
//! - `DELETE /api/upload/{slug}/{id}`  — delete upload document + files

mod create;
mod delete;
mod helpers;
mod router;
mod update;

pub use router::upload_router;
