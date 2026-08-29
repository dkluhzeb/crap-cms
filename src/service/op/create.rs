//! The `create` operation.

use crate::{
    core::DocumentFields,
    db::LocaleContext,
    service::{ServiceContext, ServiceError, WriteInput, WriteResult, create_document},
};

use crate::core::Builder;

use super::Operation;

/// Owned arguments for [`Create`]. `password` arrives already separated from
/// the data map by the codec's reserved-field handling; the service write
/// chokepoint polices it.
#[derive(Builder)]
pub struct CreateArgs {
    #[builder(required)]
    pub data: DocumentFields,
    pub password: Option<String>,
    pub locale_ctx: Option<LocaleContext>,
    pub draft: bool,
    /// Publish a mutation event for this write (request `events` flag).
    #[builder(default = true)]
    pub events: bool,
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
