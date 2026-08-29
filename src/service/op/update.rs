//! The `update` operation.

use crate::{
    core::DocumentFields,
    db::LocaleContext,
    service::{ServiceContext, ServiceError, WriteInput, WriteResult, update_document},
};

use crate::core::Builder;

use super::Operation;

/// Owned arguments for [`Update`]. Mirrors [`super::CreateArgs`] plus the
/// target document id.
#[derive(Builder)]
pub struct UpdateArgs {
    #[builder(required)]
    pub id: String,
    #[builder(required)]
    pub data: DocumentFields,
    pub password: Option<String>,
    pub locale_ctx: Option<LocaleContext>,
    pub draft: bool,
    /// Publish a mutation event for this write (request `events` flag).
    #[builder(default = true)]
    pub events: bool,
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
