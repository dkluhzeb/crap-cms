//! Input data for write operations (create/update).

use std::collections::HashMap;

use serde_json::Value;

use crate::db::LocaleContext;

/// Bundles the data parameters that callers provide for write operations,
/// reducing argument count on public API functions.
pub struct WriteInput<'a> {
    pub data: HashMap<String, String>,
    pub join_data: &'a HashMap<String, Value>,
    pub password: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub locale: Option<String>,
    pub draft: bool,
    pub ui_locale: Option<String>,
}

impl<'a> WriteInput<'a> {
    /// Create a builder with the required data and join_data fields.
    pub fn builder(
        data: HashMap<String, String>,
        join_data: &'a HashMap<String, Value>,
    ) -> WriteInputBuilder<'a> {
        WriteInputBuilder::new(data, join_data)
    }
}

/// Builder for [`WriteInput`]. Created via [`WriteInput::builder`].
pub struct WriteInputBuilder<'a> {
    pub(in crate::service) data: HashMap<String, String>,
    pub(in crate::service) join_data: &'a HashMap<String, Value>,
    pub(in crate::service) password: Option<&'a str>,
    pub(in crate::service) locale_ctx: Option<&'a LocaleContext>,
    pub(in crate::service) locale: Option<String>,
    pub(in crate::service) draft: bool,
    pub(in crate::service) ui_locale: Option<String>,
}

impl<'a> WriteInputBuilder<'a> {
    pub fn new(data: HashMap<String, String>, join_data: &'a HashMap<String, Value>) -> Self {
        Self {
            data,
            join_data,
            password: None,
            locale_ctx: None,
            locale: None,
            draft: false,
            ui_locale: None,
        }
    }

    pub fn password(mut self, password: Option<&'a str>) -> Self {
        self.password = password;
        self
    }

    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    pub fn locale(mut self, locale: Option<String>) -> Self {
        self.locale = locale;
        self
    }

    pub fn draft(mut self, draft: bool) -> Self {
        self.draft = draft;
        self
    }

    pub fn ui_locale(mut self, ui_locale: Option<String>) -> Self {
        self.ui_locale = ui_locale;
        self
    }

    pub fn build(self) -> WriteInput<'a> {
        WriteInput {
            data: self.data,
            join_data: self.join_data,
            password: self.password,
            locale_ctx: self.locale_ctx,
            locale: self.locale,
            draft: self.draft,
            ui_locale: self.ui_locale,
        }
    }
}
