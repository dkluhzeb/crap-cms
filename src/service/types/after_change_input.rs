//! Bundled parameters for after-change hook invocation.

use std::collections::HashMap;

use serde_json::Value;

use crate::core::Document;

/// Bundled parameters for after-change hook invocation.
pub(crate) struct AfterChangeInput<'a> {
    pub slug: &'a str,
    pub operation: &'a str,
    pub locale: Option<String>,
    pub is_draft: bool,
    pub req_context: HashMap<String, Value>,
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
    pub(in crate::service) req_context: HashMap<String, Value>,
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
            req_context: HashMap::new(),
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

    pub fn req_context(mut self, req_context: HashMap<String, Value>) -> Self {
        self.req_context = req_context;
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
