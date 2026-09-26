//! Validation of what several subsystems share on one Redis: the live pub/sub
//! channels, the cache namespace and the rate-limit prefix must stay apart.

use url::Url;

use crate::{
    config::{CacheBackend, CrapConfig, ErrorReport, LiveTransport, RateLimitBackend},
    core::{cache::cache_namespace, event::live_channels},
};

/// Whether two Redis URLs address the same keyspace: same host, port
/// (default 6379) and database index (default 0), ignoring credentials and
/// the scheme. `redis://h`, `redis://h:6379` and `rediss://h:6379/0` are one
/// keyspace — TLS changes how the server is reached, not which server it is.
/// Unparseable URLs fall back to an exact string comparison.
fn same_redis_instance(a: &str, b: &str) -> bool {
    match (keyspace(a), keyspace(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// The host, port and database index a Redis URL addresses.
fn keyspace(raw: &str) -> Option<(String, u16, u32)> {
    let url = Url::parse(raw).ok()?;
    let db = match url.path().trim_matches('/') {
        "" => 0,
        index => index.parse().ok()?,
    };

    Some((
        url.host_str()?.to_ascii_lowercase(),
        url.port().unwrap_or(6379),
        db,
    ))
}

/// Record a pub/sub channel that falls inside `namespace`, or the reverse.
/// An empty namespace is exempt — every name starts with it.
fn check_channel_overlap(channel: &str, namespace: &str, owner: &str, report: &mut ErrorReport) {
    if namespace.is_empty() {
        return;
    }

    if !channel.starts_with(namespace) && !namespace.starts_with(channel) {
        return;
    }

    report.push_message(format!(
        "live.channel_prefix produces the pub/sub channel {channel:?}, which overlaps {owner} \
         ({namespace:?}) on the same Redis. Choose a live channel prefix that neither contains \
         nor is contained by it, so the two namespaces stay apart."
    ));
}

impl CrapConfig {
    /// Whether any setting implies more than one node shares state: a Redis
    /// cache, event transport, or rate-limit backend.
    pub(super) fn has_multi_node_signal(&self) -> bool {
        self.cache.backend == CacheBackend::Redis
            || self.live.transport == LiveTransport::Redis
            || self.auth.rate_limit_backend == RateLimitBackend::Redis
    }

    /// The Redis a rate limiter addresses: its own URL when one is set, the
    /// cache's otherwise. One reader, so every namespace check judges the
    /// same instance the limiter will actually use.
    fn rate_limit_redis_url(&self) -> &str {
        if self.auth.rate_limit_redis_url.is_empty() {
            return self.cache.redis_url.as_str();
        }

        self.auth.rate_limit_redis_url.as_str()
    }

    /// Reject a live channel prefix that overlaps the cache or rate-limit
    /// namespace on the same Redis.
    ///
    /// Redis pub/sub is not scoped by the selected database, so the channel
    /// names are the only thing keeping two deployments on one instance
    /// apart — and a prefix that contains, or is contained by, another
    /// subsystem's namespace is the same naming collision the rate-limit
    /// check already refuses. The live transport reuses `[cache] redis_url`,
    /// so a Redis cache is by definition on the same instance.
    pub(in crate::config) fn validate_live_channel_namespace(&self, report: &mut ErrorReport) {
        if self.live.transport != LiveTransport::Redis {
            return;
        }

        let cache_shared = self.cache.backend == CacheBackend::Redis;
        let cache_ns = cache_namespace(&self.cache.prefix);

        let rl_shared = self.auth.rate_limit_backend == RateLimitBackend::Redis
            && same_redis_instance(self.rate_limit_redis_url(), self.cache.redis_url.as_str());
        let rl_ns = self.auth.rate_limit_prefix.as_str();

        for channel in live_channels(&self.live.channel_prefix) {
            if cache_shared {
                check_channel_overlap(&channel, &cache_ns, "the cache namespace", report);
            }

            if rl_shared {
                check_channel_overlap(&channel, rl_ns, "the rate-limit namespace", report);
            }
        }
    }

    /// Reject a rate-limit prefix that overlaps the cache's key namespace on
    /// the same Redis.
    ///
    /// A cache clear is a wildcard delete over `{cache.prefix}cache:*`. If
    /// rate-limit keys could fall inside that namespace, every content write
    /// would reset every lockout. The cache's own sub-namespace rules that out
    /// under the defaults; this catches an operator-chosen prefix that brings
    /// it back. An empty rate-limit prefix is exempt: its keys never start with
    /// the cache namespace.
    pub(in crate::config) fn validate_redis_namespaces(&self, report: &mut ErrorReport) {
        if self.cache.backend != CacheBackend::Redis
            || self.auth.rate_limit_backend != RateLimitBackend::Redis
        {
            return;
        }

        if !same_redis_instance(self.rate_limit_redis_url(), self.cache.redis_url.as_str()) {
            return;
        }

        let cache_ns = cache_namespace(&self.cache.prefix);
        let rl_ns = self.auth.rate_limit_prefix.as_str();

        if !rl_ns.is_empty() && (cache_ns.starts_with(rl_ns) || rl_ns.starts_with(&cache_ns)) {
            report.push_message(format!(
                "auth.rate_limit_prefix ({rl_ns:?}) overlaps the cache namespace ({cache_ns:?}) \
                 on the same Redis -- a cache clear would delete rate-limit counters and reset \
                 every lockout. Choose a prefix that neither contains nor is contained by it."
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::JwtSecret;

    const EXPLICIT_SECRET: &str = "0123456789abcdef0123456789abcdef01234567";

    /// An unset secret on a configuration that implies several nodes is a
    /// hard error: each node would generate its own.
    #[test]
    fn validate_rejects_an_empty_secret_when_redis_implies_multiple_nodes() {
        for set_signal in [
            (|c: &mut CrapConfig| c.cache.backend = CacheBackend::Redis) as fn(&mut CrapConfig),
            |c: &mut CrapConfig| c.live.transport = LiveTransport::Redis,
            |c: &mut CrapConfig| c.auth.rate_limit_backend = RateLimitBackend::Redis,
        ] {
            let mut config = CrapConfig::default();
            set_signal(&mut config);

            let err = config
                .validate()
                .expect_err("an empty secret must be refused");
            assert!(err.to_string().contains("auth.secret"), "{err}");
        }
    }

    /// With the secret set explicitly, the same configuration is accepted.
    #[test]
    fn validate_accepts_redis_with_an_explicit_secret() {
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new(EXPLICIT_SECRET);
        config.cache.backend = CacheBackend::Redis;
        config.live.transport = LiveTransport::Redis;
        config.auth.rate_limit_backend = RateLimitBackend::Redis;

        config.validate().expect("defaults on one Redis are valid");
    }

    /// A rate-limit prefix inside the cache namespace would let a cache clear
    /// wipe every lockout.
    #[test]
    fn validate_rejects_a_rate_limit_prefix_inside_the_cache_namespace() {
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new(EXPLICIT_SECRET);
        config.cache.backend = CacheBackend::Redis;
        config.auth.rate_limit_backend = RateLimitBackend::Redis;
        config.auth.rate_limit_prefix = "crap:cache:rl:".to_string();

        let err = config
            .validate()
            .expect_err("an overlapping prefix must be refused");
        assert!(err.to_string().contains("rate_limit_prefix"), "{err}");
    }

    /// Separate Redis instances cannot collide, whatever the prefixes.
    #[test]
    fn validate_allows_overlapping_prefixes_on_different_redis_instances() {
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new(EXPLICIT_SECRET);
        config.cache.backend = CacheBackend::Redis;
        config.auth.rate_limit_backend = RateLimitBackend::Redis;
        config.auth.rate_limit_prefix = "crap:cache:rl:".to_string();
        config.auth.rate_limit_redis_url = "redis://10.0.0.9:6379".into();

        config
            .validate()
            .expect("different instances do not share a keyspace");
    }

    /// The same Redis written two ways is still one keyspace.
    #[test]
    fn same_redis_instance_normalizes_port_db_and_credentials() {
        assert!(same_redis_instance("redis://h", "redis://h:6379/0"));
        assert!(same_redis_instance(
            "redis://u:pw@H:6379",
            "redis://h:6379/"
        ));
        assert!(!same_redis_instance("redis://h:6379/0", "redis://h:6379/1"));
        assert!(!same_redis_instance("redis://h", "redis://other"));
    }

    /// TLS does not make a server a different keyspace: a `rediss://` and a
    /// `redis://` URL for the same host, port and database are compared as
    /// one, so the namespace-overlap checks still apply.
    #[test]
    fn same_redis_instance_ignores_the_tls_scheme() {
        assert!(same_redis_instance(
            "rediss://u:pw@h:6380",
            "redis://h:6380/0"
        ));
        assert!(same_redis_instance("rediss://h", "rediss://h:6379/0"));
        assert!(!same_redis_instance("rediss://h:6380", "rediss://h:6381"));
    }

    /// A `rediss://` URL passes validation wherever a Redis URL is used.
    #[test]
    fn validate_accepts_rediss_urls() {
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new(EXPLICIT_SECRET);
        config.cache.backend = CacheBackend::Redis;
        config.cache.redis_url = "rediss://crap:pw@redis.internal:6380/0".into();
        config.auth.rate_limit_backend = RateLimitBackend::Redis;
        config.auth.rate_limit_redis_url = "rediss://crap:pw@redis.internal:6380/1".into();
        config.live.transport = LiveTransport::Redis;

        config
            .validate()
            .expect("rediss:// URLs are valid Redis URLs");
    }

    /// An overlapping rate-limit prefix is still refused when the cache and
    /// the rate limiter reach the same Redis through different schemes.
    #[test]
    fn validate_rejects_overlapping_prefixes_across_tls_and_plain_urls() {
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new(EXPLICIT_SECRET);
        config.cache.backend = CacheBackend::Redis;
        config.cache.redis_url = "rediss://redis.internal:6379".into();
        config.auth.rate_limit_backend = RateLimitBackend::Redis;
        config.auth.rate_limit_redis_url = "redis://redis.internal:6379/0".into();
        config.auth.rate_limit_prefix = "crap:cache:rl:".to_string();

        let err = config
            .validate()
            .expect_err("the same Redis reached over TLS is still one keyspace");
        assert!(err.to_string().contains("rate_limit_prefix"), "{err}");
    }

    /// The default live channels sit outside both key namespaces, so a
    /// Redis-everything deployment still boots untouched.
    #[test]
    fn validate_accepts_the_default_live_channels_on_a_shared_redis() {
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new(EXPLICIT_SECRET);
        config.cache.backend = CacheBackend::Redis;
        config.auth.rate_limit_backend = RateLimitBackend::Redis;
        config.live.transport = LiveTransport::Redis;

        config
            .validate()
            .expect("the default channel prefix collides with nothing");
    }

    /// A live channel prefix that swallows the cache namespace is the same
    /// naming collision an overlapping rate-limit prefix is: refused at boot.
    #[test]
    fn validate_rejects_a_live_channel_prefix_overlapping_the_cache_namespace() {
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new(EXPLICIT_SECRET);
        config.cache.backend = CacheBackend::Redis;
        config.live.transport = LiveTransport::Redis;
        config.live.channel_prefix = "crap:cache:".to_string();

        let err = config
            .validate()
            .expect_err("an overlapping channel prefix must be refused");
        assert!(err.to_string().contains("channel_prefix"), "{err}");
        assert!(err.to_string().contains("cache namespace"), "{err}");
    }

    #[test]
    fn validate_rejects_a_live_channel_prefix_overlapping_the_rate_limit_namespace() {
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new(EXPLICIT_SECRET);
        config.auth.rate_limit_backend = RateLimitBackend::Redis;
        config.live.transport = LiveTransport::Redis;
        config.live.channel_prefix = "crap:rl:".to_string();

        let err = config
            .validate()
            .expect_err("an overlapping channel prefix must be refused");
        assert!(err.to_string().contains("rate-limit namespace"), "{err}");
    }

    /// A memory transport publishes nothing to Redis, so its prefix is inert.
    #[test]
    fn validate_ignores_the_channel_prefix_without_a_redis_transport() {
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new(EXPLICIT_SECRET);
        config.cache.backend = CacheBackend::Redis;
        config.live.channel_prefix = "crap:cache:".to_string();

        config.validate().expect("an in-process transport is inert");
    }

    /// An overlapping prefix is refused even when the two URLs differ only in
    /// spelling.
    #[test]
    fn validate_rejects_overlap_when_urls_differ_only_in_spelling() {
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new(EXPLICIT_SECRET);
        config.cache.backend = CacheBackend::Redis;
        config.cache.redis_url = "redis://10.0.0.9".into();
        config.auth.rate_limit_backend = RateLimitBackend::Redis;
        config.auth.rate_limit_redis_url = "redis://10.0.0.9:6379/0".into();
        config.auth.rate_limit_prefix = "crap:cache:rl:".to_string();

        assert!(config.validate().is_err());
    }
}
