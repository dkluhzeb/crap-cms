//! The one constructor every Redis-backed subsystem opens its client
//! through (cache, rate limiter, event and invalidation transports).
//!
//! `rediss://` connects over TLS, and the TLS config is built from the
//! process-default rustls provider — so opening a client installs it
//! first ([`install_crypto_provider`]), whatever entry point got here.

use redis::{Client, RedisResult};

use crate::core::install_crypto_provider;

/// Open a Redis client for `url` (`redis://`, or `rediss://` for TLS).
///
/// Only parses the URL; connecting happens on first use.
///
/// # Errors
///
/// Returns the `redis` crate's error for a URL it cannot parse.
pub fn open_client(url: &str) -> RedisResult<Client> {
    install_crypto_provider();

    Client::open(url)
}

#[cfg(test)]
mod tests {
    use rustls::crypto::CryptoProvider;

    use super::*;

    /// A `rediss://` URL opens a client (TLS support is compiled in) and
    /// leaves a crypto provider behind for the handshake. Nothing listens
    /// on port 1, and nothing connects.
    #[test]
    fn rediss_url_opens_a_tls_client() {
        open_client("rediss://127.0.0.1:1/").expect("rediss:// must be accepted");

        assert!(CryptoProvider::get_default().is_some());
    }

    #[test]
    fn plain_redis_url_still_opens() {
        open_client("redis://127.0.0.1:1/0").expect("redis:// must be accepted");
    }

    /// Certificate verification cannot be switched off: the `#insecure`
    /// fragment needs a `redis` feature this build does not enable, so the
    /// first connection fails instead of trusting any certificate.
    #[test]
    fn insecure_fragment_cannot_skip_verification() {
        let client = open_client("rediss://127.0.0.1:1/#insecure").expect("the URL parses");

        let Err(err) = client.get_connection() else {
            panic!("an insecure TLS connection must be refused");
        };

        assert!(err.to_string().contains("insecure"), "{err}");
    }
}
