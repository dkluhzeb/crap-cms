//! Input for `get_global_document` — global document read.

use crate::db::LocaleContext;

/// Input for [`get_global_document`](crate::service::get_global_document).
pub struct GetGlobalInput<'a> {
    pub locale_ctx: Option<&'a LocaleContext>,
    pub ui_locale: Option<&'a str>,
}

impl<'a> GetGlobalInput<'a> {
    pub fn new(locale_ctx: Option<&'a LocaleContext>, ui_locale: Option<&'a str>) -> Self {
        Self {
            locale_ctx,
            ui_locale,
        }
    }
}
