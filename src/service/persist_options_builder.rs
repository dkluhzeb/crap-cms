use crate::{config::LocaleConfig, db::LocaleContext};

use super::PersistOptions;

/// Builder for [`PersistOptions`]. Created via [`PersistOptions::builder`].
#[derive(Default)]
pub struct PersistOptionsBuilder<'a> {
    pub(super) password: Option<&'a str>,
    pub(super) locale_ctx: Option<&'a LocaleContext>,
    pub(super) locale_config: Option<&'a LocaleConfig>,
    pub(super) is_draft: bool,
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
