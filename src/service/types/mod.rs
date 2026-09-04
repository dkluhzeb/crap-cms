//! Shared type definitions for the service layer.

mod after_change_input;
mod count_documents_input;
mod deadline;
mod deferred_effect;
mod email_context;
mod find_by_id_input;
mod find_documents_input;
mod get_global_input;
mod list_versions_input;
mod paginated_result;
mod pending_event;
mod pending_verification;
mod persist_options;
mod search_documents_input;
mod write_input;
mod write_result;

pub(crate) use after_change_input::AfterChangeInput;
pub use count_documents_input::CountDocumentsInput;
pub use deadline::OpDeadline;
pub(crate) use deferred_effect::flush_deferred_effects;
pub use deferred_effect::{DeferredEffect, DeferredQueue, EffectOutcome};
pub use email_context::EmailContext;
pub use find_by_id_input::FindByIdInput;
pub use find_documents_input::FindDocumentsInput;
pub use get_global_input::GetGlobalInput;
pub use list_versions_input::ListVersionsInput;
pub use paginated_result::PaginatedResult;
pub use pending_event::{EventQueue, PendingEvent};
pub(crate) use pending_event::{flush_queue, invalidate_user_streams_if_auth};
pub(crate) use pending_verification::flush_verification_queue;
pub use pending_verification::{PendingVerification, VerificationQueue};
pub use persist_options::PersistOptions;
pub use search_documents_input::SearchDocumentsInput;
pub use write_input::{WriteInput, values_from_strings};
pub use write_result::WriteResult;
