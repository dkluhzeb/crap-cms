//! Input for `get_global_document` — global document read.

use crate::db::LocaleContext;

/// Input for [`get_global_document`](crate::service::get_global_document).
pub struct GetGlobalInput<'a> {
    pub locale_ctx: Option<&'a LocaleContext>,
    pub ui_locale: Option<&'a str>,
    /// Whether this read may see unpublished (draft) global content. Defaults
    /// to `false` so public surfaces never serve a global that has been
    /// unpublished; the admin edit form opts in with `true`.
    pub include_drafts: bool,
}

impl<'a> GetGlobalInput<'a> {
    #[must_use]
    pub fn new(locale_ctx: Option<&'a LocaleContext>, ui_locale: Option<&'a str>) -> Self {
        Self {
            locale_ctx,
            ui_locale,
            include_drafts: false,
        }
    }

    /// Allow this read to see unpublished (draft) global content. Used by the
    /// admin edit form so an unpublished global remains editable.
    #[must_use]
    pub fn include_drafts(mut self, include_drafts: bool) -> Self {
        self.include_drafts = include_drafts;
        self
    }
}
