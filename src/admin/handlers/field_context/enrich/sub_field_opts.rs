//! Bundled inputs for sub-field enrichment functions.

use std::collections::HashMap;

use crate::core::Builder;

/// Bundled parameters for sub-field enrichment functions (`sub_array`,
/// `sub_blocks`, `sub_row_collapsible`, `sub_tabs`,
/// `build_enriched_sub_field_context`) to avoid too many arguments.
#[derive(Builder)]
pub struct SubFieldOpts<'a> {
    pub locale_locked: bool,
    pub non_default_locale: bool,
    pub depth: usize,
    #[builder(required)]
    pub errors: &'a HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults_to_false_flags_and_zero_depth() {
        let errors = HashMap::new();
        let opts = SubFieldOpts::builder(&errors).build();
        assert!(!opts.locale_locked);
        assert!(!opts.non_default_locale);
        assert_eq!(opts.depth, 0);
    }

    /// `locale_locked` and `non_default_locale` are both `bool` — set them to
    /// distinct values so a swapped assignment in `build()` surfaces.
    #[test]
    fn builder_wires_each_field_to_its_own_slot() {
        let errors = HashMap::new();
        let opts = SubFieldOpts::builder(&errors)
            .locale_locked(true)
            .non_default_locale(false)
            .depth(3)
            .build();

        assert!(opts.locale_locked);
        assert!(!opts.non_default_locale);
        assert_eq!(opts.depth, 3);
    }
}
