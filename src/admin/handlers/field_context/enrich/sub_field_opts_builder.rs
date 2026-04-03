use std::collections::HashMap;

use crate::admin::handlers::field_context::enrich::SubFieldOpts;

/// Builder for [`SubFieldOpts`].
pub struct SubFieldOptsBuilder<'a> {
    pub(super) locale_locked: bool,
    pub(super) non_default_locale: bool,
    pub(super) depth: usize,
    pub(super) errors: &'a HashMap<String, String>,
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
