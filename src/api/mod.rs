//! gRPC API server (Tonic) implementing the ContentAPI service.

pub mod handlers;
pub(crate) mod rate_limit;
pub mod server;
pub mod upload;

/// Generated gRPC content service types.
pub mod content {
    tonic::include_proto!("crap");

    /// File descriptor set for gRPC reflection.
    pub const FILE_DESCRIPTOR_SET: &[u8] =
        tonic::include_file_descriptor_set!("content_descriptor");
}
