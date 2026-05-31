//! Bundled parameters for after-change hook invocation.

use crate::core::{Document, ReqContext};

/// Bundled parameters for after-change hook invocation.
pub(crate) struct AfterChangeInput<'a> {
    pub slug: &'a str,
    pub operation: &'a str,
    pub locale: Option<String>,
    pub is_draft: bool,
    pub req_context: ReqContext,
    pub user: Option<&'a Document>,
    pub ui_locale: Option<&'a str>,
}

impl<'a> AfterChangeInput<'a> {
    /// Create a builder with the required slug and operation.
    pub fn builder(slug: &'a str, operation: &'a str) -> AfterChangeInputBuilder<'a> {
        AfterChangeInputBuilder::new(slug, operation)
    }
}

/// Builder for [`AfterChangeInput`]. Created via [`AfterChangeInput::builder`].
pub(crate) struct AfterChangeInputBuilder<'a> {
    pub(in crate::service) slug: &'a str,
    pub(in crate::service) operation: &'a str,
    pub(in crate::service) locale: Option<String>,
    pub(in crate::service) is_draft: bool,
    pub(in crate::service) req_context: ReqContext,
    pub(in crate::service) user: Option<&'a Document>,
    pub(in crate::service) ui_locale: Option<&'a str>,
}

impl<'a> AfterChangeInputBuilder<'a> {
    pub fn new(slug: &'a str, operation: &'a str) -> Self {
        Self {
            slug,
            operation,
            locale: None,
            is_draft: false,
            req_context: ReqContext::new(),
            user: None,
            ui_locale: None,
        }
    }

    pub fn locale(mut self, locale: Option<String>) -> Self {
        self.locale = locale;
        self
    }

    pub fn draft(mut self, is_draft: bool) -> Self {
        self.is_draft = is_draft;
        self
    }

    pub fn req_context(mut self, req_context: impl Into<ReqContext>) -> Self {
        self.req_context = req_context.into();
        self
    }

    pub fn user(mut self, user: Option<&'a Document>) -> Self {
        self.user = user;
        self
    }

    pub fn ui_locale(mut self, ui_locale: Option<&'a str>) -> Self {
        self.ui_locale = ui_locale;
        self
    }

    pub fn build(self) -> AfterChangeInput<'a> {
        AfterChangeInput {
            slug: self.slug,
            operation: self.operation,
            locale: self.locale,
            is_draft: self.is_draft,
            req_context: self.req_context,
            user: self.user,
            ui_locale: self.ui_locale,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults_are_empty() {
        let aci = AfterChangeInput::builder("posts", "update").build();
        assert_eq!(aci.slug, "posts");
        assert_eq!(aci.operation, "update");
        assert!(aci.locale.is_none());
        assert!(!aci.is_draft);
        assert!(aci.user.is_none());
        assert!(aci.ui_locale.is_none());
    }

    /// `slug` and `operation` are distinct strings, so a swap in `build()`
    /// would surface here.
    #[test]
    fn builder_wires_each_field() {
        let aci = AfterChangeInput::builder("posts", "create")
            .locale(Some("de".into()))
            .draft(true)
            .ui_locale(Some("en"))
            .build();
        assert_eq!(aci.slug, "posts");
        assert_eq!(aci.operation, "create");
        assert_eq!(aci.locale.as_deref(), Some("de"));
        assert!(aci.is_draft);
        assert_eq!(aci.ui_locale, Some("en"));
    }
}
