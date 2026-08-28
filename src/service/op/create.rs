//! The `create` operation.

use crate::{
    core::DocumentFields,
    db::LocaleContext,
    service::{ServiceContext, ServiceError, WriteInput, WriteResult, create_document},
};

use super::Operation;

/// Owned arguments for [`Create`]. `password` arrives already separated from
/// the data map by the codec's reserved-field handling; the service write
/// chokepoint polices it.
pub struct CreateArgs {
    pub data: DocumentFields,
    pub password: Option<String>,
    pub locale_ctx: Option<LocaleContext>,
    pub draft: bool,
    /// Publish a mutation event for this write (request `events` flag).
    pub events: bool,
}

impl CreateArgs {
    #[must_use]
    pub fn builder(data: DocumentFields) -> CreateArgsBuilder {
        CreateArgsBuilder::new(data)
    }
}

/// Builder for [`CreateArgs`].
pub struct CreateArgsBuilder {
    data: DocumentFields,
    password: Option<String>,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    events: bool,
}

impl CreateArgsBuilder {
    fn new(data: DocumentFields) -> Self {
        Self {
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
    pub fn build(self) -> CreateArgs {
        CreateArgs {
            data: self.data,
            password: self.password,
            locale_ctx: self.locale_ctx,
            draft: self.draft,
            events: self.events,
        }
    }
}

/// Create a document with the full write lifecycle (validation, hooks,
/// password policy, ref counting, events).
pub enum Create {}

impl Operation for Create {
    type Args = CreateArgs;
    type Output = WriteResult;

    const NAME: &'static str = "create";

    const READS_VIA_CONTEXT: bool = false;

    fn emit_events(args: &Self::Args) -> bool {
        args.events
    }

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let CreateArgs {
            data,
            password,
            locale_ctx,
            draft,
            events: _,
        } = args;

        create_document(
            ctx,
            WriteInput::builder(data)
                .password(password.as_deref())
                .locale_ctx(locale_ctx.as_ref())
                .draft(draft)
                .ui_locale(ctx.ui_locale.clone())
                .build(),
        )
    }
}
