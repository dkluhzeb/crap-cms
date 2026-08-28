//! The `update` operation.

use crate::{
    core::DocumentFields,
    db::LocaleContext,
    service::{ServiceContext, ServiceError, WriteInput, WriteResult, update_document},
};

use super::Operation;

/// Owned arguments for [`Update`]. Mirrors [`super::CreateArgs`] plus the
/// target document id.
pub struct UpdateArgs {
    pub id: String,
    pub data: DocumentFields,
    pub password: Option<String>,
    pub locale_ctx: Option<LocaleContext>,
    pub draft: bool,
    /// Publish a mutation event for this write (request `events` flag).
    pub events: bool,
}

impl UpdateArgs {
    #[must_use]
    pub fn builder(id: impl Into<String>, data: DocumentFields) -> UpdateArgsBuilder {
        UpdateArgsBuilder::new(id.into(), data)
    }
}

/// Builder for [`UpdateArgs`].
pub struct UpdateArgsBuilder {
    id: String,
    data: DocumentFields,
    password: Option<String>,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    events: bool,
}

impl UpdateArgsBuilder {
    fn new(id: String, data: DocumentFields) -> Self {
        Self {
            id,
            data,
            password: None,
            locale_ctx: None,
            draft: false,
            events: true,
        }
    }

    #[must_use]
    pub fn password(mut self, password: Option<String>) -> Self {
        self.password = password;
        self
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
    pub fn build(self) -> UpdateArgs {
        UpdateArgs {
            id: self.id,
            data: self.data,
            password: self.password,
            locale_ctx: self.locale_ctx,
            draft: self.draft,
            events: self.events,
        }
    }
}

/// Update a document with the full write lifecycle.
pub enum Update {}

impl Operation for Update {
    type Args = UpdateArgs;
    type Output = WriteResult;

    const NAME: &'static str = "update";

    const READS_VIA_CONTEXT: bool = false;

    fn emit_events(args: &Self::Args) -> bool {
        args.events
    }

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let UpdateArgs {
            id,
            data,
            password,
            locale_ctx,
            draft,
            events: _,
        } = args;

        update_document(
            ctx,
            &id,
            WriteInput::builder(data)
                .password(password.as_deref())
                .locale_ctx(locale_ctx.as_ref())
                .draft(draft)
                .ui_locale(ctx.ui_locale.clone())
                .build(),
        )
    }
}
