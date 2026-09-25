//! Request preconditions on a served upload (RFC 9110 §13.1.1, §13.1.4):
//! `If-Match` and `If-Unmodified-Since`.
//!
//! Both are judged first — before the cache validators (`If-None-Match` /
//! `If-Modified-Since`) and before any `Range` — and a failed one answers
//! `412 Precondition Failed`, so a client that pins a representation (a
//! resumed download checking it still has the same file, a sync tool) never
//! receives a different version. The local and remote paths share this one
//! judgment; each supplies the validators it has: the remote path its entity
//! tag and `Last-Modified` from the object's metadata, the local path its
//! file's `Last-Modified` (it sends no entity tag).

use axum::{
    body::Body,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::DateTime;

use crate::admin::handlers::uploads::headers::{ConditionalHeaders, ServeHeaders};

/// Whether an `If-Match` value matches `etag` under the **strong** comparison
/// the header calls for: `*` matches any current representation, a listed tag
/// only when it is identical and strong — a weak tag (`W/"…"`) never matches,
/// and nothing matches a representation without an entity tag.
fn if_match_passes(header: &str, etag: Option<&str>) -> bool {
    let header = header.trim();

    if header == "*" {
        return true;
    }

    let Some(etag) = etag else {
        return false;
    };

    header
        .split(',')
        .map(str::trim)
        .any(|candidate| !candidate.starts_with("W/") && candidate == etag)
}

/// Whether an `If-Unmodified-Since` date admits a representation last
/// modified at `last_modified`. The header is ignored — the condition passes —
/// when its date is invalid or the representation has no modification date.
fn unmodified_since_passes(header: &str, last_modified: Option<&str>) -> bool {
    let Ok(since) = DateTime::parse_from_rfc2822(header.trim()) else {
        return true;
    };

    let Some(modified) = last_modified.and_then(|m| DateTime::parse_from_rfc2822(m.trim()).ok())
    else {
        return true;
    };

    modified <= since
}

/// Whether the request's preconditions pass for a representation with these
/// validators; `false` answers `412`. `If-Match` decides alone when present
/// (an invalid value fails it); `If-Unmodified-Since` applies only without
/// one.
pub(super) fn preconditions_pass(
    conditional: &ConditionalHeaders,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> bool {
    if let Some(header) = conditional.if_match.as_ref() {
        return header.to_str().is_ok_and(|h| if_match_passes(h, etag));
    }

    let Some(header) = conditional.if_unmodified_since.as_ref() else {
        return true;
    };

    let Ok(value) = header.to_str() else {
        return true;
    };

    unmodified_since_passes(value, last_modified)
}

/// `412 Precondition Failed`, with no body and the headers every served
/// upload carries.
pub(super) fn precondition_failed(headers: &ServeHeaders<'_>) -> Response {
    let mut response = (StatusCode::PRECONDITION_FAILED, Body::empty()).into_response();

    headers.apply(&mut response);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATE: &str = "Sun, 06 Nov 1994 08:49:37 GMT";
    const EARLIER: &str = "Sat, 05 Nov 1994 08:49:37 GMT";

    /// Whether a request carrying `if_match` / `since` passes against a
    /// representation with `etag` / `modified`.
    fn passes(
        if_match: Option<&str>,
        since: Option<&str>,
        etag: Option<&str>,
        modified: Option<&str>,
    ) -> bool {
        let conditional = ConditionalHeaders {
            if_match: if_match.map(|v| v.parse().unwrap()),
            if_unmodified_since: since.map(|v| v.parse().unwrap()),
            ..ConditionalHeaders::default()
        };

        preconditions_pass(&conditional, etag, modified)
    }

    #[test]
    fn no_preconditions_pass() {
        assert!(passes(None, None, None, None));
    }

    /// `If-Match` compares strongly: the same strong tag passes, a different
    /// or weak one fails, `*` passes, and a representation without a tag fails
    /// every listed tag.
    #[test]
    fn if_match_uses_the_strong_comparison() {
        let tag = Some("\"abc\"");

        assert!(passes(Some("\"abc\""), None, tag, None));
        assert!(passes(Some("\"x\", \"abc\""), None, tag, None));
        assert!(!passes(Some("\"x\""), None, tag, None));
        assert!(!passes(Some("W/\"abc\""), None, tag, None));
        assert!(passes(Some("*"), None, tag, None));
        assert!(passes(Some("*"), None, None, None));
        assert!(!passes(Some("\"abc\""), None, None, None));
    }

    /// A representation modified after the date fails; one at or before it
    /// passes; an invalid date or a missing modification date is ignored.
    #[test]
    fn if_unmodified_since_compares_the_modification_date() {
        assert!(passes(None, Some(DATE), None, Some(DATE)));
        assert!(passes(None, Some(DATE), None, Some(EARLIER)));
        assert!(!passes(None, Some(EARLIER), None, Some(DATE)));
        assert!(passes(None, Some("not a date"), None, Some(DATE)));
        assert!(passes(None, Some(EARLIER), None, None));
    }

    /// With `If-Match` present, `If-Unmodified-Since` is not evaluated — in
    /// either direction.
    #[test]
    fn if_unmodified_since_is_ignored_when_if_match_is_present() {
        let tag = Some("\"abc\"");

        assert!(passes(Some("\"abc\""), Some(EARLIER), tag, Some(DATE)));
        assert!(!passes(Some("\"x\""), Some(DATE), tag, Some(EARLIER)));
    }

    #[test]
    fn a_failed_precondition_is_a_412_with_the_shared_headers() {
        let headers = ServeHeaders::new("application/pdf", "private, no-store");
        let response = precondition_failed(&headers);

        assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
        assert_eq!(
            response.headers().get("cache-control").unwrap(),
            "private, no-store"
        );
    }
}
