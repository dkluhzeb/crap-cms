//! Bundled inputs for [`enrich_field_contexts`](super::enrich_field_contexts).

use std::collections::HashMap;

/// Bundled parameters for `enrich_field_contexts` to avoid too many arguments.
pub struct EnrichOptions<'a> {
    pub filter_hidden: bool,
    pub non_default_locale: bool,
    pub errors: &'a HashMap<String, String>,
    pub doc_id: Option<&'a str>,
}

impl<'a> EnrichOptions<'a> {
    pub fn builder(errors: &'a HashMap<String, String>) -> EnrichOptionsBuilder<'a> {
        EnrichOptionsBuilder::new(errors)
    }
}

/// Builder for [`EnrichOptions`].
pub struct EnrichOptionsBuilder<'a> {
    filter_hidden: bool,
    non_default_locale: bool,
    errors: &'a HashMap<String, String>,
    doc_id: Option<&'a str>,
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

    /// Take `Option<&str>` so an optional caller-side `doc_id` flows through
    /// without an `if let` ceremony at the call site.
    pub fn doc_id(mut self, v: Option<&'a str>) -> Self {
        self.doc_id = v;
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
