//! The `update_global` and `unpublish_global` operations.

use crate::{
    core::{Document, DocumentFields},
    db::LocaleContext,
    service::{
        ServiceContext, ServiceError, WriteInput, WriteResult, unpublish_global_document,
        update_global_document,
    },
};

use super::Operation;

/// Owned arguments for [`UpdateGlobal`]. Mirrors [`super::CreateArgs`]
/// without a password (globals have no auth).
pub struct UpdateGlobalArgs {
    pub data: DocumentFields,
    pub locale_ctx: Option<LocaleContext>,
    pub draft: bool,
    /// Publish a mutation event for this write (request `events` flag).
    pub events: bool,
}

impl UpdateGlobalArgs {
    #[must_use]
    pub fn builder(data: DocumentFields) -> UpdateGlobalArgsBuilder {
        UpdateGlobalArgsBuilder::new(data)
    }
}

/// Builder for [`UpdateGlobalArgs`].
pub struct UpdateGlobalArgsBuilder {
    data: DocumentFields,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    events: bool,
}

impl UpdateGlobalArgsBuilder {
    fn new(data: DocumentFields) -> Self {
        Self {
            data,
            locale_ctx: None,
            draft: false,
            events: true,
        }
    }

    #[must_use]
    pub fn locale_ctx(mut self, locale_ctx: Option<LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    #[must_use]
    pub fn draft(mut self, draft: bool) -> Self {
        self.draft = draft;
        self
    }

    #[must_use]
    pub fn events(mut self, events: bool) -> Self {
        self.events = events;
        self
    }

    #[must_use]
    pub fn build(self) -> UpdateGlobalArgs {
        UpdateGlobalArgs {
            data: self.data,
            locale_ctx: self.locale_ctx,
            draft: self.draft,
            events: self.events,
        }
    }
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
        } = args;

        update_global_document(
            ctx,
            WriteInput::builder(data)
                .locale_ctx(locale_ctx.as_ref())
                .draft(draft)
                .ui_locale(ctx.ui_locale.clone())
                .build(),
        )
    }
}

/// Owned arguments for [`UnpublishGlobal`].
pub struct UnpublishGlobalArgs {
    /// Publish a mutation event for this write (request `events` flag).
    pub events: bool,
}

impl Default for UnpublishGlobalArgs {
    fn default() -> Self {
        Self { events: true }
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

    fn run(ctx: &ServiceContext<'_>, _args: Self::Args) -> Result<Self::Output, ServiceError> {
        unpublish_global_document(ctx)
    }
}
