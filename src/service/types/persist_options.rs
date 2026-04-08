//! Optional parameters for persist operations.

use crate::{config::LocaleConfig, db::LocaleContext};

/// Optional parameters for the persist_create / persist_update operations.
#[derive(Default)]
pub struct PersistOptions<'a> {
    pub password: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub locale_config: Option<&'a LocaleConfig>,
    pub is_draft: bool,
}

impl<'a> PersistOptions<'a> {
    /// Create a builder with all fields defaulted.
    pub fn builder() -> PersistOptionsBuilder<'a> {
        PersistOptionsBuilder::new()
    }
}

/// Builder for [`PersistOptions`]. Created via [`PersistOptions::builder`].
#[derive(Default)]
pub struct PersistOptionsBuilder<'a> {
    pub(in crate::service) password: Option<&'a str>,
    pub(in crate::service) locale_ctx: Option<&'a LocaleContext>,
    pub(in crate::service) locale_config: Option<&'a LocaleConfig>,
    pub(in crate::service) is_draft: bool,
}

impl<'a> PersistOptionsBuilder<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn password(mut self, password: Option<&'a str>) -> Self {
        self.password = password;
        self
    }

    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    pub fn locale_config(mut self, locale_config: &'a LocaleConfig) -> Self {
        self.locale_config = Some(locale_config);
        self
    }

    pub fn draft(mut self, is_draft: bool) -> Self {
        self.is_draft = is_draft;
        self
    }

    pub fn build(self) -> PersistOptions<'a> {
        PersistOptions {
            password: self.password,
            locale_ctx: self.locale_ctx,
            locale_config: self.locale_config,
            is_draft: self.is_draft,
        }
    }
}
