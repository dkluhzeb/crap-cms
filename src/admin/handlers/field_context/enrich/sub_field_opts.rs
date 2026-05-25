//! Bundled inputs for sub-field enrichment functions.

use std::collections::HashMap;

/// Bundled parameters for sub-field enrichment functions (`sub_array`,
/// `sub_blocks`, `sub_row_collapsible`, `sub_tabs`,
/// `build_enriched_sub_field_context`) to avoid too many arguments.
pub struct SubFieldOpts<'a> {
    pub locale_locked: bool,
    pub non_default_locale: bool,
    pub depth: usize,
    pub errors: &'a HashMap<String, String>,
}

impl<'a> SubFieldOpts<'a> {
    pub fn builder(errors: &'a HashMap<String, String>) -> SubFieldOptsBuilder<'a> {
        SubFieldOptsBuilder::new(errors)
    }
}

/// Builder for [`SubFieldOpts`].
pub struct SubFieldOptsBuilder<'a> {
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
    errors: &'a HashMap<String, String>,
}

impl<'a> SubFieldOptsBuilder<'a> {
    pub fn new(errors: &'a HashMap<String, String>) -> Self {
        Self {
            locale_locked: false,
            non_default_locale: false,
            depth: 0,
            errors,
        }
    }

    pub fn locale_locked(mut self, v: bool) -> Self {
        self.locale_locked = v;
        self
    }

    pub fn non_default_locale(mut self, v: bool) -> Self {
        self.non_default_locale = v;
        self
    }

    pub fn depth(mut self, v: usize) -> Self {
        self.depth = v;
        self
    }

    pub fn build(self) -> SubFieldOpts<'a> {
        SubFieldOpts {
            locale_locked: self.locale_locked,
            non_default_locale: self.non_default_locale,
            depth: self.depth,
            errors: self.errors,
        }
    }
}
