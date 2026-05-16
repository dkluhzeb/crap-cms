//! gRPC API server (Tonic) implementing the `ContentAPI` service.
//!
//! ## Submodule layout
//!
//! - `server.rs` -- gRPC server bootstrap. Public entry is
//!   [`server::start`] (re-exported flat as [`start`]) which spawns
//!   the Tonic server with the configured `ContentService` impl.
//!   `GrpcStartParams` bundles the server's runtime dependencies.
//! - `handlers/` -- one file per RPC handler family, all wired into
//!   the `ContentService` impl (`crud`, `bulk`, `globals`, `auth`,
//!   `versions`, `subscribe`, `upload`, …). `content_service.rs`
//!   composes the impls into the actual Tonic service.
//! - `upload/` -- multipart upload handlers (separate from the gRPC
//!   handlers because uploads are HTTP, not gRPC).
//! - `rate_limit.rs` -- per-IP gRPC request limiter (`pub(crate)`,
//!   used internally by `server`).
//! - `content` -- generated Tonic types from `proto/content.proto`,
//!   built at compile time via `build.rs`.
//!
//! ## Conventions
//!
//! - Service implementations return `Result<_, Status>` directly.
//!   `tokio::spawn_blocking` bodies live in named `*_blocking`
//!   functions taking typed `*BlockingInput` structs (no inline
//!   multi-statement closures).
//! - `From<ServiceError> for Status` (in
//!   `handlers/collection/error_mapping.rs`) is the single mapping
//!   layer between the service-error vocabulary and gRPC status
//!   codes; handlers use `.map_err(Status::from)` and
//!   `?`-propagate.

pub mod handlers;
pub mod rate_limit;
pub mod server;
pub mod upload;

pub use server::{GrpcStartParams, start};

/// Generated gRPC content service types.
pub mod content {
    // Generated from `proto/content.proto` by tonic_prost_build at build time.
    // Doc comments inside the generated code come from the .proto source and
    // are written for cross-language consumers (Go/Python/TS); they intentionally
    // don't use Rust-specific Markdown conventions like backticks for identifiers
    // or `# Errors` sections on Result-returning functions. The codegen also
    // emits `Default::default()` in struct literals, which clippy::pedantic flags.
    #![allow(
        clippy::doc_markdown,
        clippy::missing_errors_doc,
        clippy::default_trait_access,
        clippy::too_many_lines
    )]

    tonic::include_proto!("crap");

    /// File descriptor set for gRPC reflection.
    pub const FILE_DESCRIPTOR_SET: &[u8] =
        tonic::include_file_descriptor_set!("content_descriptor");
}
