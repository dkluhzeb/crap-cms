//! Bundled inputs for [`enrich_field_contexts`](super::enrich_field_contexts).

use std::collections::HashMap;

use crate::core::{Builder, Document};

/// Bundled parameters for `enrich_field_contexts` to avoid too many arguments.
#[derive(Builder)]
pub struct EnrichOptions<'a> {
    pub filter_hidden: bool,
    pub non_default_locale: bool,
    #[builder(required)]
    pub errors: &'a HashMap<String, String>,
    pub doc_id: Option<&'a str>,
    /// The viewer, so relationship/join/upload label reads are access-gated
    /// (a viewer must not learn the title/existence of targets they can't read).
    pub user: Option<&'a Document>,
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
