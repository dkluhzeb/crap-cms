//! Validation of the `[pagination]`, `[depth]` and `[jobs]` limits (and the
//! `[hooks]` VM pool sizes the job runner draws on).

use tracing::warn;

use crate::config::{CrapConfig, ErrorReport};

/// `serde_json`'s default deserialization recursion limit — the round-trip
/// ceiling for stored JSON (see the `max_nesting_depth` check).
const SERDE_RECURSION_LIMIT: usize = 128;

impl CrapConfig {
    /// Validate pagination limits. The two are compared only once both are
    /// positive, so a bad one is one problem.
    pub(in crate::config) fn validate_pagination(&self, report: &mut ErrorReport) {
        let (default_limit, max_limit) = (self.pagination.default_limit, self.pagination.max_limit);

        if default_limit <= 0 {
            report.push_message("pagination.default_limit must be > 0");
        }

        if max_limit <= 0 {
            report.push_message("pagination.max_limit must be > 0");
        }

        if default_limit > 0 && max_limit > 0 && default_limit > max_limit {
            report.push_message(format!(
                "pagination.default_limit ({default_limit}) must be <= pagination.max_limit \
                 ({max_limit})"
            ));
        }
    }

    /// Validate depth/population limits.
    pub(in crate::config) fn validate_depth(&self, report: &mut ErrorReport) {
        if self.depth.default_depth < 0 {
            report.push_message("depth.default_depth must be >= 0");
        }

        if self.depth.max_depth < 0 {
            report.push_message("depth.max_depth must be >= 0");
        }

        if self.depth.max_depth == 0 {
            warn!("depth.max_depth = 0 -- all depth/populate requests will be capped to 0");
        }

        if self.depth.default_depth > self.depth.max_depth {
            warn!(
                "depth.default_depth ({}) exceeds depth.max_depth ({}) -- requests will be capped",
                self.depth.default_depth, self.depth.max_depth
            );
        }

        if self.depth.max_nesting_depth == 0 {
            report.push_message("depth.max_nesting_depth must be >= 1 (0 rejects all nested data)");
        }

        self.warn_on_nesting_depth();
    }

    /// Advisory warnings on the data-nesting ceiling.
    fn warn_on_nesting_depth(&self) {
        // The data-nesting ceiling must accommodate the data that population
        // produces: a document populated to `max_depth` nests at least that
        // deep, so a smaller ceiling would reject your own legitimately-deep
        // data at Lua↔JSON conversion time.
        if let Ok(max_depth) = usize::try_from(self.depth.max_depth)
            && self.depth.max_nesting_depth < max_depth
        {
            warn!(
                "depth.max_nesting_depth ({}) is below depth.max_depth ({}) -- data populated to max_depth may exceed the nesting limit and fail to convert",
                self.depth.max_nesting_depth, self.depth.max_depth
            );
        }

        // Data nested deeper than serde's parse limit can be built in memory and
        // serialized, but cannot be parsed back from stored JSON — effectively
        // write-only. Going beyond it would require a custom `Deserializer`
        // recursion limit at every user-JSON parse site.
        if self.depth.max_nesting_depth > SERDE_RECURSION_LIMIT {
            warn!(
                "depth.max_nesting_depth ({}) exceeds the JSON parser recursion limit ({SERDE_RECURSION_LIMIT}) -- data nested deeper than {SERDE_RECURSION_LIMIT} can be built but will not parse back from stored JSON",
                self.depth.max_nesting_depth
            );
        }
    }

    /// Validate job scheduler settings.
    pub(in crate::config) fn validate_jobs(&self, report: &mut ErrorReport) {
        if self.hooks.vm_pool_size == 0 {
            report.push_message("hooks.vm_pool_size must be > 0");
        }

        if self.hooks.max_vm_pool_size == 0 {
            report.push_message("hooks.max_vm_pool_size must be > 0");
        }

        if self.jobs.max_concurrent == 0 {
            warn!("jobs.max_concurrent = 0 -- no jobs will be executed");
        }

        if self.jobs.poll_interval == 0 {
            report.push_message("jobs.poll_interval must be > 0");
        }

        if self.jobs.cron_interval == 0 {
            report.push_message("jobs.cron_interval must be > 0");
        }

        if self.jobs.heartbeat_interval == 0 {
            report.push_message("jobs.heartbeat_interval must be > 0");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_vm_pool_size_zero_errors() {
        let mut config = CrapConfig::default();
        config.hooks.vm_pool_size = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("vm_pool_size"));
    }

    #[test]
    fn validate_max_concurrent_zero_warns_but_passes() {
        let mut config = CrapConfig::default();
        config.jobs.max_concurrent = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_poll_interval_zero_errors() {
        let mut config = CrapConfig::default();
        config.jobs.poll_interval = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("poll_interval"));
    }

    #[test]
    fn validate_cron_interval_zero_errors() {
        let mut config = CrapConfig::default();
        config.jobs.cron_interval = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("cron_interval"));
    }

    #[test]
    fn validate_heartbeat_interval_zero_errors() {
        let mut config = CrapConfig::default();
        config.jobs.heartbeat_interval = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("heartbeat_interval"));
    }

    #[test]
    fn validate_max_depth_zero_warns_but_passes() {
        let mut config = CrapConfig::default();
        config.depth.max_depth = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_max_nesting_depth_zero_errors() {
        let mut config = CrapConfig::default();
        config.depth.max_nesting_depth = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("max_nesting_depth"));
    }

    #[test]
    fn validate_max_nesting_depth_below_max_depth_warns_but_passes() {
        // A nesting ceiling under the population depth is a misconfiguration
        // (populated data could exceed it) but is surfaced as a warning, not a
        // hard failure.
        let mut config = CrapConfig::default();
        config.depth.max_depth = 20;
        config.depth.max_nesting_depth = 5;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_pagination_default_limit_zero_errors() {
        let mut config = CrapConfig::default();
        config.pagination.default_limit = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_limit"));
    }

    #[test]
    fn validate_pagination_default_limit_negative_errors() {
        let mut config = CrapConfig::default();
        config.pagination.default_limit = -5;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_limit"));
    }

    #[test]
    fn validate_pagination_max_limit_zero_errors() {
        let mut config = CrapConfig::default();
        config.pagination.max_limit = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("max_limit"));
    }

    #[test]
    fn validate_pagination_default_exceeds_max_errors() {
        let mut config = CrapConfig::default();
        config.pagination.default_limit = 100;
        config.pagination.max_limit = 50;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_limit"));
        assert!(err.to_string().contains("max_limit"));
    }

    #[test]
    fn validate_depth_negative_default_errors() {
        let mut config = CrapConfig::default();
        config.depth.default_depth = -1;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_depth"));
    }

    #[test]
    fn validate_depth_negative_max_errors() {
        let mut config = CrapConfig::default();
        config.depth.max_depth = -1;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("max_depth"));
    }
}
