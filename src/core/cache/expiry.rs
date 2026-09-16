//! How long a cached entry may live.

/// Ceiling on a Redis cache entry's lifetime when `[cache] max_age_secs` is
/// `0` — the default, meaning "no periodic clear; write-through invalidation
/// only".
///
/// At that setting the memory backend is still bounded two ways: entries die
/// with the process, and `max_entries` caps the store. A Redis store has
/// neither — it outlives every node and has no entry cap — so without a
/// ceiling a key written during a quiet stretch would sit there indefinitely.
/// Every write clears the whole cache namespace, so a day is far longer than
/// the gap between clears in any deployment that caches at all: the ceiling
/// costs no hit rate, it only stops a shared store from growing without limit.
const REDIS_TTL_CEILING_SECS: u64 = 24 * 60 * 60;

/// The TTL a Redis cache entry is written with, in seconds. Never `0`: a
/// Redis entry always expires on its own, which is why the periodic full
/// clear skips this backend (wiping a store every node shares, once per node,
/// would cut entry lifetime by the node count).
///
/// `max_age_secs > 0` is the operator's own staleness bound and wins
/// outright, including when it is longer than the ceiling — that is an
/// explicit choice. `0` falls back to the ceiling above.
#[must_use]
pub fn redis_entry_ttl_secs(max_age_secs: u64) -> u64 {
    if max_age_secs > 0 {
        max_age_secs
    } else {
        REDIS_TTL_CEILING_SECS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default `max_age_secs = 0` must still produce an expiring key —
    /// the periodic clear skips Redis on exactly that promise.
    #[test]
    fn the_default_still_yields_an_expiring_entry() {
        assert_eq!(redis_entry_ttl_secs(0), REDIS_TTL_CEILING_SECS);
        assert!(redis_entry_ttl_secs(0) > 0);
    }

    #[test]
    fn a_configured_max_age_wins_in_both_directions() {
        assert_eq!(redis_entry_ttl_secs(60), 60);

        let longer = REDIS_TTL_CEILING_SECS * 7;
        assert_eq!(
            redis_entry_ttl_secs(longer),
            longer,
            "an explicit staleness bound is not clamped to the ceiling"
        );
    }
}
