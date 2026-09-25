//! Input for `get_global_document` — global document read.

use crate::{db::LocaleContext, service::read::post_process::PostProcessOpts};

/// Input for [`get_global_document`](crate::service::get_global_document).
pub struct GetGlobalInput<'a> {
    pub locale_ctx: Option<&'a LocaleContext>,
    pub ui_locale: Option<&'a str>,
    /// Whether this read may see unpublished (draft) global content. Defaults
    /// to `false` so public surfaces never serve a global that has been
    /// unpublished; the admin edit form opts in with `true`.
    pub include_drafts: bool,
    /// Relationship population depth, as for a collection read. Defaults to
    /// `0` (ids only); a surface resolves the requested depth against the
    /// `[depth]` config before it gets here.
    pub depth: i32,
}

impl<'a> GetGlobalInput<'a> {
    #[must_use]
    pub fn new(locale_ctx: Option<&'a LocaleContext>, ui_locale: Option<&'a str>) -> Self {
        Self {
            locale_ctx,
            ui_locale,
            include_drafts: false,
            depth: 0,
        }
    }

    /// Allow this read to see unpublished (draft) global content. Used by the
    /// admin edit form so an unpublished global remains editable.
    #[must_use]
    pub fn include_drafts(mut self, include_drafts: bool) -> Self {
        self.include_drafts = include_drafts;
        self
    }

    /// Populate the global's relationship and upload fields to `depth`.
    #[must_use]
    pub fn depth(mut self, depth: i32) -> Self {
        self.depth = depth;
        self
    }
}

impl PostProcessOpts for GetGlobalInput<'_> {
    fn depth(&self) -> i32 {
        self.depth
    }
    fn include_drafts(&self) -> bool {
        // A draft read shows the targets' drafts where the reader may see
        // them, as a collection's draft read does.
        self.include_drafts
    }
    fn hydrate(&self) -> bool {
        false
    }
    fn select(&self) -> Option<&[String]> {
        None
    }
    fn locale_ctx(&self) -> Option<&LocaleContext> {
        self.locale_ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SAFE-DEFAULT GUARD: a global read populates nothing and shows no
    /// unpublished content unless the surface asks for it.
    #[test]
    fn a_global_read_defaults_to_ids_and_published_content() {
        let input = GetGlobalInput::new(None, None);

        assert_eq!(PostProcessOpts::depth(&input), 0);
        assert!(!PostProcessOpts::include_drafts(&input));
    }
}
