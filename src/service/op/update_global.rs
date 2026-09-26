//! The `update_global` and `unpublish_global` operations.

use crate::{
    core::{Document, DocumentFields},
    db::LocaleContext,
    service::{
        ServiceContext, ServiceError, WriteInput, WriteResult, unpublish_global_document,
        update_global_document,
    },
};

use crate::core::Builder;

use super::{Operation, locale::write_locale_ctx};

/// Owned arguments for [`UpdateGlobal`]. Mirrors [`super::CreateArgs`]
/// without a password (globals have no auth).
#[derive(Builder)]
pub struct UpdateGlobalArgs {
    #[builder(required)]
    pub data: DocumentFields,
    pub locale_ctx: Option<LocaleContext>,
    pub draft: bool,
    /// Publish a mutation event for this write (request `events` flag).
    #[builder(default = true)]
    pub events: bool,
    /// The global's revision the caller last read (`expected_revision`). Set,
    /// the update is refused with a conflict when the global has been written
    /// since; `None` writes unconditionally.
    pub expected_revision: Option<i64>,
}

/// Update a global document with the full write lifecycle.
pub enum UpdateGlobal {}

impl Operation for UpdateGlobal {
    type Args = UpdateGlobalArgs;
    type Output = WriteResult;

    const NAME: &'static str = "update_global";

    const READS_VIA_CONTEXT: bool = false;

    fn emit_events(args: &Self::Args) -> bool {
        args.events
    }

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let UpdateGlobalArgs {
            data,
            locale_ctx,
            draft,
            events: _,
            expected_revision,
        } = args;

        let locale_ctx = write_locale_ctx(locale_ctx)?;

        update_global_document(
            ctx,
            WriteInput::builder(data)
                .locale_ctx(locale_ctx.as_ref())
                .draft(draft)
                .expected_revision(expected_revision)
                .build(),
        )
    }
}

/// Owned arguments for [`UnpublishGlobal`].
pub struct UnpublishGlobalArgs {
    /// Publish a mutation event for this write (request `events` flag).
    pub events: bool,
    /// The global's revision the caller last read. Set, the unpublish is
    /// refused with a conflict when the global has been written since.
    pub expected_revision: Option<i64>,
}

impl UnpublishGlobalArgs {
    #[must_use]
    pub fn new(events: bool, expected_revision: Option<i64>) -> Self {
        Self {
            events,
            expected_revision,
        }
    }
}

impl Default for UnpublishGlobalArgs {
    fn default() -> Self {
        Self::new(true, None)
    }
}

/// Revert a global to draft status. The service gate rejects unpublish on a
/// non-versioned global with an explicit error on every surface.
pub enum UnpublishGlobal {}

impl Operation for UnpublishGlobal {
    type Args = UnpublishGlobalArgs;
    type Output = Document;

    const NAME: &'static str = "unpublish_global";

    const READS_VIA_CONTEXT: bool = false;

    fn emit_events(args: &Self::Args) -> bool {
        args.events
    }

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        unpublish_global_document(ctx, args.expected_revision)
    }
}
