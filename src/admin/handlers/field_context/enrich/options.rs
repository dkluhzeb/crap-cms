//! Bundled inputs for [`enrich_field_contexts`](super::enrich_field_contexts).

use std::collections::HashMap;

use crate::core::Document;

/// Bundled parameters for `enrich_field_contexts` to avoid too many arguments.
pub struct EnrichOptions<'a> {
    pub filter_hidden: bool,
    pub non_default_locale: bool,
    pub errors: &'a HashMap<String, String>,
    pub doc_id: Option<&'a str>,
    /// The viewer, so relationship/join/upload label reads are access-gated
    /// (a viewer must not learn the title/existence of targets they can't read).
    pub user: Option<&'a Document>,
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
    user: Option<&'a Document>,
}

impl<'a> EnrichOptionsBuilder<'a> {
    pub fn new(errors: &'a HashMap<String, String>) -> Self {
        Self {
            filter_hidden: false,
            non_default_locale: false,
            errors,
            doc_id: None,
            user: None,
        }
    }

    /// The viewer, for access-gating label reads.
    pub fn user(mut self, v: Option<&'a Document>) -> Self {
        self.user = v;
        self
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
            user: self.user,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults_are_false_and_none() {
        let errors = HashMap::new();
        let opts = EnrichOptions::builder(&errors).build();
        assert!(!opts.filter_hidden);
        assert!(!opts.non_default_locale);
        assert!(opts.doc_id.is_none());
    }

    /// `filter_hidden` and `non_default_locale` are both `bool` — set them to
    /// distinct values so a swapped assignment in `build()` surfaces.
    #[test]
    fn builder_wires_each_bool_to_its_own_slot() {
        let mut errors = HashMap::new();
        errors.insert("title".to_string(), "required".to_string());

        let opts = EnrichOptions::builder(&errors)
            .filter_hidden(true)
            .non_default_locale(false)
            .doc_id(Some("doc-1"))
            .build();

        assert!(opts.filter_hidden);
        assert!(!opts.non_default_locale);
        assert_eq!(opts.doc_id, Some("doc-1"));
        assert_eq!(
            opts.errors.get("title").map(String::as_str),
            Some("required")
        );
    }
}
