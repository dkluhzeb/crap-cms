//! The `get_global` operation.

use crate::{
    core::Document,
    db::LocaleContext,
    service::{GetGlobalInput, ServiceContext, ServiceError, get_global_document},
};

use crate::core::Builder;

use super::Operation;

/// Owned arguments for [`GetGlobal`].
#[derive(Builder)]
pub struct GetGlobalArgs {
    pub locale_ctx: Option<LocaleContext>,
    /// Whether this read may see unpublished (draft) global content. Public
    /// surfaces default to `false`; the admin edit form opts in.
    pub include_drafts: bool,
}

/// Read a global document with the full read lifecycle (view union,
/// published-snapshot fallback for unpublished globals, field stripping).
pub enum GetGlobal {}

impl Operation for GetGlobal {
    type Args = GetGlobalArgs;
    type Output = Document;

    const NAME: &'static str = "get_global";

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        // `ui_locale` comes off the context (resolved by the op entry / set by
        // the Lua and admin codecs) — the one source every op reads, instead
        // of a per-op Args twin that only two codecs remembered to fill.
        let input = GetGlobalInput::new(args.locale_ctx.as_ref(), ctx.ui_locale.as_deref())
            .include_drafts(args.include_drafts);

        get_global_document(ctx, &input)
    }
}
