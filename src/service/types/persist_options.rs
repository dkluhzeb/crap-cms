//! Optional parameters for persist operations.

use serde_json::{Map, Value};

use crate::{config::LocaleConfig, db::LocaleContext};

/// Optional parameters for the `persist_create` / `persist_update` operations.
#[derive(Default)]
pub struct PersistOptions<'a> {
    pub password: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub locale_config: Option<&'a LocaleConfig>,
    pub is_draft: bool,
    /// The pending draft snapshot a publish makes live, once the publisher's
    /// field-level write access has been applied to it.
    ///
    /// Set only by a publishing update on a localized collection: the write's
    /// own data already carries the draft's values for the locale it targets,
    /// and this is what carries the draft's OTHER locales and its shared
    /// values onto the row. The persist step writes it back inside its
    /// ref-count bracket, because the write-back moves relationships too.
    pub pending_draft: Option<&'a Map<String, Value>>,
}

impl<'a> PersistOptions<'a> {
    /// Create a builder with all fields defaulted.
    #[must_use]
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
    pub(in crate::service) pending_draft: Option<&'a Map<String, Value>>,
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

    pub fn locale_config(mut self, locale_config: Option<&'a LocaleConfig>) -> Self {
        self.locale_config = locale_config;
        self
    }

    pub fn draft(mut self, is_draft: bool) -> Self {
        self.is_draft = is_draft;
        self
    }

    /// Set the pending draft snapshot this publish makes live (see the field).
    pub fn pending_draft(mut self, pending_draft: Option<&'a Map<String, Value>>) -> Self {
        self.pending_draft = pending_draft;
        self
    }

    pub fn build(self) -> PersistOptions<'a> {
        PersistOptions {
            password: self.password,
            locale_ctx: self.locale_ctx,
            locale_config: self.locale_config,
            is_draft: self.is_draft,
            pending_draft: self.pending_draft,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_default_is_empty() {
        let o = PersistOptions::builder().build();
        assert!(o.password.is_none());
        assert!(o.locale_ctx.is_none());
        assert!(o.locale_config.is_none());
        assert!(!o.is_draft);
        assert!(o.pending_draft.is_none());
    }

    /// A publish hands the persist step the draft it makes live; every other
    /// write leaves the slot empty.
    #[test]
    fn builder_carries_the_pending_draft_snapshot() {
        let snapshot = Map::new();

        let o = PersistOptions::builder()
            .pending_draft(Some(&snapshot))
            .build();

        assert!(o.pending_draft.is_some());
    }

    #[test]
    fn builder_passes_through_password_and_draft() {
        let o = PersistOptions::builder()
            .password(Some("pw"))
            .draft(true)
            .build();
        assert_eq!(o.password, Some("pw"));
        assert!(o.is_draft);
    }
}
