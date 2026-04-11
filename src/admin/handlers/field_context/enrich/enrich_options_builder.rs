use std::collections::HashMap;

use crate::admin::handlers::field_context::enrich::EnrichOptions;

/// Builder for [`EnrichOptions`].
pub struct EnrichOptionsBuilder<'a> {
    pub(super) filter_hidden: bool,
    pub(super) non_default_locale: bool,
    pub(super) errors: &'a HashMap<String, String>,
    pub(super) doc_id: Option<&'a str>,
}

impl<'a> EnrichOptionsBuilder<'a> {
    pub fn new(errors: &'a HashMap<String, String>) -> Self {
        Self {
            filter_hidden: false,
            non_default_locale: false,
            errors,
            doc_id: None,
        }
    }

    pub fn filter_hidden(mut self, v: bool) -> Self {
        self.filter_hidden = v;
        self
    }

    pub fn non_default_locale(mut self, v: bool) -> Self {
        self.non_default_locale = v;
        self
    }

    pub fn doc_id(mut self, v: &'a str) -> Self {
        self.doc_id = Some(v);
        self
    }

    pub fn build(self) -> EnrichOptions<'a> {
        EnrichOptions {
            filter_hidden: self.filter_hidden,
            non_default_locale: self.non_default_locale,
            errors: self.errors,
            doc_id: self.doc_id,
        }
    }
}
