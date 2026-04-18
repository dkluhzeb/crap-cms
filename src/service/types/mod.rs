//! Shared type definitions for the service layer.

mod after_change_input;
mod count_documents_input;
mod find_by_id_input;
mod find_documents_input;
mod get_global_input;
mod list_versions_input;
mod paginated_result;
mod persist_options;
mod search_documents_input;
mod service_context;
mod write_input;
mod write_result;

pub(crate) use after_change_input::AfterChangeInput;
pub use count_documents_input::{CountDocumentsInput, CountDocumentsInputBuilder};
pub use find_by_id_input::{FindByIdInput, FindByIdInputBuilder};
pub use find_documents_input::{FindDocumentsInput, FindDocumentsInputBuilder};
pub use get_global_input::GetGlobalInput;
pub use list_versions_input::ListVersionsInput;
pub use paginated_result::PaginatedResult;
pub use persist_options::{PersistOptions, PersistOptionsBuilder};
pub use search_documents_input::SearchDocumentsInput;
pub use service_context::{
    Def, EmailContext, EventQueue, ServiceContext, ServiceContextBuilder, VerificationQueue,
    flush_queue, flush_verification_queue,
};
pub use write_input::{WriteInput, WriteInputBuilder};
pub use write_result::WriteResult;
