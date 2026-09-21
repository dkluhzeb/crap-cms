//! Redis pub/sub channel layout for live events.
//!
//! Both channels hang off one configurable prefix. Pub/sub is not scoped by
//! the selected Redis database, so two deployments pointed at one Redis would
//! otherwise cross-deliver each other's mutation events — full document
//! payloads — whenever their slugs coincide. The prefix is what keeps them
//! apart, exactly as `[cache] prefix` keeps their cache keys apart.
//!
//! Deliberately outside the `redis` feature gate, so the layout — and the
//! startup check that rejects a prefix overlapping the cache or rate-limit
//! namespace — is always compiled and tested.

/// Default prefix for both live channels. Chosen so the channel names are
/// exactly what they were before the prefix existed.
pub const DEFAULT_LIVE_CHANNEL_PREFIX: &str = "crap:";

/// Segment appended to the prefix for the mutation-event channel.
const EVENT_CHANNEL_SUFFIX: &str = "events";

/// Segment appended to the prefix for the user-invalidation channel.
const INVALIDATION_CHANNEL_SUFFIX: &str = "invalidations";

/// The channel mutation events are published on: `{prefix}events`.
#[must_use]
pub fn event_channel(prefix: &str) -> String {
    format!("{prefix}{EVENT_CHANNEL_SUFFIX}")
}

/// The channel user-invalidation signals are published on:
/// `{prefix}invalidations`.
#[must_use]
pub fn invalidation_channel(prefix: &str) -> String {
    format!("{prefix}{INVALIDATION_CHANNEL_SUFFIX}")
}

/// Every channel a node with this prefix publishes to or subscribes to — the
/// list the startup namespace check walks.
#[must_use]
pub fn live_channels(prefix: &str) -> [String; 2] {
    [event_channel(prefix), invalidation_channel(prefix)]
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_LIVE_CHANNEL_PREFIX, event_channel, invalidation_channel, live_channels};

    /// The default must keep the channel names existing single-deployment
    /// setups already use — a prefix that renamed them would silently split a
    /// running cluster in two on upgrade.
    #[test]
    fn the_default_prefix_keeps_todays_channel_names() {
        assert_eq!(event_channel(DEFAULT_LIVE_CHANNEL_PREFIX), "crap:events");
        assert_eq!(
            invalidation_channel(DEFAULT_LIVE_CHANNEL_PREFIX),
            "crap:invalidations"
        );
    }

    /// The publisher and the subscriber must never address different
    /// channels: both derive theirs from these functions, so one prefix
    /// yields exactly one pair.
    #[test]
    fn a_prefix_yields_exactly_one_channel_pair() {
        let [events, invalidations] = live_channels("app-a:");

        assert_eq!(events, event_channel("app-a:"));
        assert_eq!(invalidations, invalidation_channel("app-a:"));
        assert_ne!(events, invalidations);
        assert_ne!(events, event_channel("app-b:"));
    }

    #[test]
    fn a_custom_prefix_moves_both_channels_together() {
        let channels = live_channels("staging:");

        assert_eq!(channels[0], "staging:events");
        assert_eq!(channels[1], "staging:invalidations");
    }
}
