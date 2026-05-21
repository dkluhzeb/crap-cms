//! Core domain types: collections, fields, documents, registry, auth,
//! uploads, jobs, validation, events, and the cross-request cache /
//! rate-limit backends.
//!
//! ## Submodule layout + import policy
//!
//! Two flavours of submodule:
//!
//! - **Leaf modules** (`auth`, `collection`, `condition`, `document`,
//!   `document_fields`, `document_id`, `field`, `job`, `registry`,
//!   `req_context`, `slug`, `timezone`, `validate`) -- one or two
//!   tightly-coupled types each. Their public types are re-exported
//!   flat at `crate::core::*`; external callers use the short path.
//!   Builders stay one level deeper, accessed via `Type::builder()`.
//! - **Namespace modules** (`cache`, `email`, `event`, `rate_limit`,
//!   `upload`) -- a domain with a trait + impls + factory + domain
//!   contexts. The module prefix carries semantic meaning at call
//!   sites (`cache::create_cache(...)` reads as "build a cache";
//!   `email::MfaCodeEmailContext` says "email render context").
//!   External callers reach traits/factories/impl-classes/contexts
//!   through `crate::core::<domain>::Foo`.
//!
//! **The exception inside the namespace modules:** the `Shared*`
//! handle types (Arc-wrapped trait objects that app code actually
//! holds at runtime) are flat-re-exported at `crate::core::*`, since
//! the `Shared*` prefix already documents the role. Same for the
//! bare runtime values `MutationEvent`, `MutationEventInput`,
//! `EventReceiver` -- self-documenting names with 60+ external
//! call sites each.
//!
//! ## Conventions
//!
//! - `#[derive(Default)]` everywhere it works; manual `Default` only
//!   when fields have non-trivial defaults.
//! - `>2`-field structs use builder pattern; `<=2`-field use `new()`.
//! - Builders colocate with their struct in a single file with the
//!   tests at the bottom.

pub mod auth;
pub mod cache;
pub mod collection;
pub mod condition;
pub mod document;
pub mod document_fields;
pub mod document_id;
pub mod email;
pub mod event;
pub mod field;
pub mod job;
pub mod rate_limit;
pub mod registry;
pub mod req_context;
pub mod richtext;
pub mod slug;
pub mod timezone;
pub mod upload;
pub mod validate;

// Leaf-module types: flat re-export.
pub use auth::{AuthUser, Claims, HashedPassword, JwtSecret, ResetTokenError};
pub use collection::{
    Access, CollectionDefinition, GlobalDefinition, Hooks, IndexDefinition, Labels, LiveMode,
    LiveSetting, VersionsConfig,
};
pub use condition::{ConditionExpr, ConditionOp, ConditionRow};
pub use document::Document;
pub use document_fields::DocumentFields;
pub use document_id::DocumentId;
pub use field::{
    BlockDefinition, FieldAccess, FieldAdmin, FieldAdminBuilder, FieldAdminLabels, FieldDefinition,
    FieldDefinitionBuilder, FieldHookFn, FieldHooks, FieldTab, FieldType, FieldWidth, JoinConfig,
    LocalizedString, McpFieldConfig, PickerAppearance, RelationshipConfig, SelectOption,
    ValidateFunction, to_title_case, validate_template_name,
};
pub use job::{JobDefinition, JobLabels, JobRun, JobStatus};
pub(crate) use registry::RegistryRead;
pub use registry::{Registry, SharedRegistry};
pub use req_context::ReqContext;
pub use richtext::RichtextNodeDef;
pub use slug::Slug;
pub use validate::{FieldError, ValidationError};

// Namespace-module exception: `Shared*` handle types (Arc-wrapped
// trait objects) and self-documenting runtime values are flat. Their
// own names already carry the domain (`SharedTokenProvider`,
// `SharedEventTransport`, `MutationEvent`), so the `auth::` /
// `event::` prefix would be redundant. Traits, factory fns, impl
// classes, and domain-bound contexts stay inside their namespace
// (e.g. `cache::CacheBackend`, `cache::create_cache`,
// `cache::MemoryCache`, `email::MfaCodeEmailContext`).
pub use auth::{SharedPasswordProvider, SharedTokenProvider};
pub use cache::SharedCache;
pub use email::SharedEmailProvider;
pub use event::{
    EventReceiver, MutationEvent, MutationEventInput, SharedEventTransport,
    SharedInvalidationTransport,
};
pub use rate_limit::SharedRateLimitBackend;
pub use upload::SharedStorage;
