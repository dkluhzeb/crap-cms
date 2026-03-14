//! gRPC API server (Tonic) implementing the ContentAPI service.

pub mod rate_limit;
pub mod server;
pub mod server_builder;
pub mod service;
pub mod upload;

pub use server_builder::GrpcStartParamsBuilder;

/// Generated gRPC content service types.
pub mod content {
    tonic::include_proto!("crap");

    /// File descriptor set for gRPC reflection.
    pub const FILE_DESCRIPTOR_SET: &[u8] =
        tonic::include_file_descriptor_set!("content_descriptor");
}
