//! Builder for [`UnpublishCtx`].

use super::write::UnpublishCtx;

use crate::core::{CollectionDefinition, Document};

/// Builder for [`UnpublishCtx`].
pub(super) struct UnpublishCtxBuilder<'a> {
    collection: &'a str,
    id: &'a str,
    def: &'a CollectionDefinition,
    run_hooks: bool,
    locale_str: Option<&'a str>,
    hook_user: Option<&'a Document>,
    hook_ui_locale: Option<&'a str>,
}

impl<'a> UnpublishCtxBuilder<'a> {
    pub(super) fn new(collection: &'a str, id: &'a str, def: &'a CollectionDefinition) -> Self {
        Self {
            collection,
            id,
            def,
            run_hooks: true,
            locale_str: None,
            hook_user: None,
            hook_ui_locale: None,
        }
    }

    pub(super) fn run_hooks(mut self, v: bool) -> Self {
        self.run_hooks = v;
        self
    }

    pub(super) fn locale_str(mut self, v: Option<&'a str>) -> Self {
        self.locale_str = v;
        self
    }

    pub(super) fn hook_user(mut self, v: Option<&'a Document>) -> Self {
        self.hook_user = v;
        self
    }

    pub(super) fn hook_ui_locale(mut self, v: Option<&'a str>) -> Self {
        self.hook_ui_locale = v;
        self
    }

    pub(super) fn build(self) -> UnpublishCtx<'a> {
        UnpublishCtx {
            collection: self.collection,
            id: self.id,
            def: self.def,
            run_hooks: self.run_hooks,
            locale_str: self.locale_str,
            hook_user: self.hook_user,
            hook_ui_locale: self.hook_ui_locale,
        }
    }
}
