//! The process-wide rustls crypto provider.
//!
//! rustls 0.23 builds a client config from a process-default
//! [`CryptoProvider`] unless the caller names one. It can only pick that
//! default itself when exactly one provider backend is compiled in, and
//! this dependency tree compiles both `ring` and `aws-lc-rs` — so any
//! client that relies on the default (the Redis `rediss://` connector
//! does) panics unless one has been installed first.
//!
//! [`install_crypto_provider`] installs `ring` once. The binary calls it
//! before dispatching any command, and every Redis client is opened
//! through a path that calls it again, so an embedder of the library
//! cannot reach a TLS handshake without a provider either.

use rustls::crypto::{CryptoProvider, ring};

/// Install `ring` as the process-default rustls [`CryptoProvider`].
///
/// Idempotent and cheap: once a default exists — from an earlier call, or
/// one an embedding application installed on purpose — it is kept.
pub fn install_crypto_provider() {
    if CryptoProvider::get_default().is_some() {
        return;
    }

    // Fails only when another thread installed a default between the check
    // and here; a default then exists, which is all this function promises.
    let _ = ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rustls::{ClientConfig, RootCertStore};

    use super::*;

    /// After installation a process default exists, and a second call
    /// neither panics nor replaces it.
    #[test]
    fn install_is_idempotent_and_leaves_a_default() {
        install_crypto_provider();

        let first = CryptoProvider::get_default().expect("a default provider is installed");

        install_crypto_provider();

        let second = CryptoProvider::get_default().expect("the default is still installed");

        assert!(
            Arc::ptr_eq(first, second),
            "the installed default must be kept"
        );
    }

    /// Building a client config from the process default — what the Redis
    /// TLS connector does — does not panic once the provider is installed.
    #[test]
    fn client_config_builds_from_the_process_default() {
        install_crypto_provider();

        let config = ClientConfig::builder()
            .with_root_certificates(RootCertStore::empty())
            .with_no_client_auth();

        assert!(config.alpn_protocols.is_empty());
    }
}
