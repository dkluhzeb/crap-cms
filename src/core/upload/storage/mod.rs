//! Storage backend abstraction for upload files.
//!
//! Provides a trait-based backend system: `local` (default filesystem),
//! `s3` (S3-compatible, feature-flagged), and `custom` (Lua-delegated).
//!
//! The [`StorageBackend`] trait + [`SharedStorage`] type alias live in the
//! sibling [`backend`] module; sub-modules (`local`, `s3`, `custom`)
//! implement the trait. The factory ([`create_storage`]) picks the
//! implementation by config.

mod backend;
mod custom;
mod factory;
mod local;
#[cfg(feature = "s3-storage")]
mod s3;

pub use crate::config::UploadConfig;
pub use backend::{SharedStorage, StorageBackend, StorageNotFound};
pub use custom::CustomStorage;
pub use factory::{create_storage, create_storage_with_lease};
pub use local::LocalStorage;

/// URL path prefix of the built-in upload-serve proxy route.
///
/// The value stored in a document's `url` / `{size}_url` columns is always
/// `served_url(key)` — this backend-agnostic proxy path — regardless of storage
/// backend, because every backend serves bytes through this route. (Direct
/// S3/CDN links were removed with the dead `public_url` limb; a signed-URL
/// scheme is the planned way to bypass the proxy.)
pub const SERVED_URL_PREFIX: &str = "/uploads/";

/// The canonical served URL for a storage `key`, as stored in a document's
/// url-bearing columns and matched by the serve access-gate. One source shared
/// by the write path, the serve gate, and the delete path so they can't disagree.
#[must_use]
pub fn served_url(key: &str) -> String {
    format!("{SERVED_URL_PREFIX}{key}")
}

/// Recover the storage key from a served URL produced by [`served_url`], or
/// `None` when the value isn't a served-proxy URL (e.g. an external `image_url`).
#[must_use]
pub fn key_from_served_url(url: &str) -> Option<&str> {
    url.strip_prefix(SERVED_URL_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::{key_from_served_url, served_url};

    #[test]
    fn served_url_round_trips_with_key_extraction() {
        let key = "posts/abc123_photo.jpg";
        let url = served_url(key);
        assert_eq!(url, "/uploads/posts/abc123_photo.jpg");
        assert_eq!(key_from_served_url(&url), Some(key));
    }

    #[test]
    fn key_from_served_url_rejects_non_proxy_urls() {
        // An external absolute URL (e.g. a raw `image_url` field) is not a
        // served-proxy path and must not be mistaken for a storage key.
        assert_eq!(key_from_served_url("https://cdn.example.com/x.jpg"), None);
    }
}
