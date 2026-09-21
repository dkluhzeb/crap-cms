//! Header construction shared by every upload-serve path.
//!
//! The local backend answers through `tower_http::services::ServeFile` and the
//! remote backends build their response by hand, but both finish here: one
//! place decides `Cache-Control`, `Content-Disposition`, `Accept-Ranges`, the
//! SVG sandbox CSP and `Vary`, so the two cannot drift apart. The conditional
//! (`If-None-Match` / `If-Modified-Since`) and `Range` parsing used by the
//! remote path lives here too, next to the headers it answers with.

use std::fmt::Write as _;

use axum::{
    body::Body,
    http::{
        HeaderValue, Request,
        header::{
            ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY,
            IF_MODIFIED_SINCE, IF_NONE_MATCH, RANGE, VARY,
        },
    },
    response::Response,
};
use chrono::DateTime;
use sha2::{Digest, Sha256};

use crate::core::upload::{ByteRange, RangedObject, original_filename};

/// Conditional / range headers taken off the original request. Forwarded to
/// `ServeFile` on the local path and answered directly on the remote one.
pub(super) struct ConditionalHeaders {
    pub(super) range: Option<HeaderValue>,
    pub(super) if_none_match: Option<HeaderValue>,
    pub(super) if_modified_since: Option<HeaderValue>,
}

/// Invisible format characters that reorder or hide text: bidi embeddings,
/// overrides and isolates, zero-width characters, and the byte-order mark.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
    )
}

/// Percent-encode a value for an RFC 5987 `ext-value`: `attr-char`s stay, every
/// other UTF-8 byte becomes `%XX`.
fn encode_rfc5987(value: &str) -> String {
    let mut out = String::with_capacity(value.len());

    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }

    out
}

/// `name` with every character that could break the header or disguise the name
/// replaced: a control character (the extension is not sanitized upstream, so a
/// crafted upload can smuggle a CRLF here) or an invisible bidi override, which
/// can disguise the extension the user sees.
fn visible_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control() || is_invisible_format(c) {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// The ASCII form of `name` the quoted `filename` parameter carries; a name that
/// isn't ASCII travels in full in RFC 6266 `filename*`.
fn ascii_fallback(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Determine `Content-Disposition` for a file based on its MIME type.
///
/// Images (except SVG) are inline. SVGs and non-image files get attachment
/// to prevent stored XSS. `stored` is the name the file has in storage
/// (`{id}_{original}`); the download is offered under the original name, and a
/// name that doesn't have that shape is offered without one at all.
pub(super) fn content_disposition(mime: &str, stored: Option<&str>) -> String {
    if mime.starts_with("image/") && mime != "image/svg+xml" {
        return "inline".to_string();
    }

    let Some(name) = stored.and_then(original_filename) else {
        return "attachment".to_string();
    };

    let visible = visible_name(name);
    let fallback = ascii_fallback(&visible);

    if fallback == visible {
        return format!("attachment; filename=\"{fallback}\"");
    }

    format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{}",
        encode_rfc5987(&visible)
    )
}

/// The security / caching headers every served upload carries, whatever
/// backend produced the bytes.
///
/// `mime` and `cache_control` are required; `varied` and the stored filename
/// are attached by the caller that has them.
pub(super) struct ServeHeaders<'a> {
    mime: &'a str,
    cache_control: &'a str,
    varied: bool,
    stored_name: Option<&'a str>,
}

impl<'a> ServeHeaders<'a> {
    /// Start from the two headers every response needs.
    pub(super) fn new(mime: &'a str, cache_control: &'a str) -> Self {
        Self {
            mime,
            cache_control,
            varied: false,
            stored_name: None,
        }
    }

    /// Mark the response as content-negotiated (`Vary: Accept`).
    #[must_use]
    pub(super) fn varied(mut self, varied: bool) -> Self {
        self.varied = varied;
        self
    }

    /// The name the file has in storage, from which the download name is
    /// recovered.
    #[must_use]
    pub(super) fn stored_name(mut self, stored_name: Option<&'a str>) -> Self {
        self.stored_name = stored_name;
        self
    }

    /// The content type these headers describe.
    pub(super) fn mime(&self) -> &str {
        self.mime
    }

    /// Apply to a response built by any backend path.
    pub(super) fn apply(&self, response: &mut Response) {
        let disposition = content_disposition(self.mime, self.stored_name);

        let headers = response.headers_mut();

        // Never `.expect` on a data-derived header value: the disposition
        // sanitizes control characters, but fall back to a bare `attachment`
        // rather than panic the request task if an unexpected byte survives.
        headers.insert(
            CONTENT_DISPOSITION,
            disposition
                .parse()
                .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
        );

        headers.insert(
            CACHE_CONTROL,
            self.cache_control
                .parse()
                .unwrap_or_else(|_| HeaderValue::from_static("private, no-store")),
        );

        // Every backend can answer a range now, so every response advertises
        // it — including the ones that carry no range.
        headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));

        if self.mime == "image/svg+xml" {
            headers.insert(
                CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("sandbox; default-src 'none'"),
            );
        }

        if self.varied {
            headers.insert(VARY, HeaderValue::from_static("Accept"));
        }
    }
}

pub(super) fn extract_conditional_headers(req: &Request<Body>) -> ConditionalHeaders {
    ConditionalHeaders {
        range: req.headers().get(RANGE).cloned(),
        if_none_match: req.headers().get(IF_NONE_MATCH).cloned(),
        if_modified_since: req.headers().get(IF_MODIFIED_SINCE).cloned(),
    }
}

pub(super) fn build_serve_request(headers: &ConditionalHeaders) -> Request<Body> {
    let mut builder = Request::builder().uri("/");

    if let Some(ref v) = headers.range {
        builder = builder.header(RANGE, v);
    }

    if let Some(ref v) = headers.if_none_match {
        builder = builder.header(IF_NONE_MATCH, v);
    }

    if let Some(ref v) = headers.if_modified_since {
        builder = builder.header(IF_MODIFIED_SINCE, v);
    }

    builder.body(Body::empty()).expect("static request builder")
}

/// The single byte range a request asks for.
///
/// `None` means "serve the whole representation": no `Range` header, a header
/// we cannot parse, an inverted range (RFC 9110 requires an invalid range set
/// to be ignored, not refused), or a multi-range request — `multipart/byteranges`
/// is deliberately not implemented.
pub(super) fn parse_range(header: Option<&HeaderValue>) -> Option<ByteRange> {
    let spec = header?.to_str().ok()?.trim().strip_prefix("bytes=")?.trim();

    if spec.contains(',') {
        return None;
    }

    let (start, end) = spec.split_once('-')?;
    let (start, end) = (start.trim(), end.trim());

    if start.is_empty() {
        return Some(ByteRange::Suffix(end.parse().ok()?));
    }

    let start: u64 = start.parse().ok()?;

    if end.is_empty() {
        return Some(ByteRange::from_start(start));
    }

    let end: u64 = end.parse().ok()?;

    (start <= end).then(|| ByteRange::inclusive(start, end))
}

/// A strong entity tag for a remote object.
///
/// The backend's own tag is authoritative when it reports one (S3 returns the
/// object's `ETag`). Otherwise the identity a stored upload actually has —
/// its key, its size and its last-modified stamp — is hashed into one, so
/// conditional requests still work against backends that report nothing.
pub(super) fn remote_etag(key: &str, object: &RangedObject) -> String {
    if let Some(etag) = object.etag.as_deref() {
        let etag = etag.trim_matches('"');

        if !etag.is_empty() {
            return format!("\"{etag}\"");
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hasher.update(b"\n");
    hasher.update(object.total_size.unwrap_or_default().to_le_bytes());
    hasher.update(b"\n");
    hasher.update(object.last_modified.as_deref().unwrap_or("").as_bytes());

    let mut tag = String::from("\"");
    for byte in &hasher.finalize()[..16] {
        let _ = write!(tag, "{byte:02x}");
    }
    tag.push('"');

    tag
}

/// Whether an `If-None-Match` value matches `etag`. `*` matches anything that
/// exists, and a weak tag (`W/"…"`) matches its strong form — a weak
/// comparison is exactly what a conditional GET calls for.
fn etag_matches(header: &str, etag: &str) -> bool {
    let header = header.trim();

    if header == "*" {
        return true;
    }

    header
        .split(',')
        .map(|candidate| candidate.trim().trim_start_matches("W/").trim())
        .any(|candidate| candidate == etag)
}

/// Whether the object's `Last-Modified` is at or before the request's
/// `If-Modified-Since`. An unparseable date on either side is not a match, so
/// the file is served rather than wrongly reported unchanged.
fn not_modified_since(header: &str, last_modified: &str) -> bool {
    let Ok(since) = DateTime::parse_from_rfc2822(header.trim()) else {
        return false;
    };

    let Ok(modified) = DateTime::parse_from_rfc2822(last_modified.trim()) else {
        return false;
    };

    modified <= since
}

/// Whether the viewer's cached copy is still current, so the response is a
/// `304` with no body.
///
/// `If-None-Match` decides alone when present — RFC 9110 gives the entity tag
/// precedence over the date — and `If-Modified-Since` only applies when no tag
/// was sent.
pub(super) fn is_fresh(
    conditional: &ConditionalHeaders,
    etag: &str,
    last_modified: Option<&str>,
) -> bool {
    if let Some(header) = conditional.if_none_match.as_ref() {
        return header.to_str().is_ok_and(|h| etag_matches(h, etag));
    }

    let (Some(header), Some(modified)) = (conditional.if_modified_since.as_ref(), last_modified)
    else {
        return false;
    };

    header
        .to_str()
        .is_ok_and(|h| not_modified_since(h, modified))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conditional(
        range: Option<&str>,
        if_none_match: Option<&str>,
        since: Option<&str>,
    ) -> ConditionalHeaders {
        ConditionalHeaders {
            range: range.map(|v| v.parse().unwrap()),
            if_none_match: if_none_match.map(|v| v.parse().unwrap()),
            if_modified_since: since.map(|v| v.parse().unwrap()),
        }
    }

    #[test]
    fn extract_conditional_headers_captures_range() {
        let req = Request::builder()
            .uri("/")
            .header(RANGE, "bytes=0-99")
            .header(IF_NONE_MATCH, "\"abc\"")
            .body(Body::empty())
            .unwrap();
        let headers = extract_conditional_headers(&req);
        assert_eq!(headers.range.unwrap().to_str().unwrap(), "bytes=0-99");
        assert_eq!(headers.if_none_match.unwrap().to_str().unwrap(), "\"abc\"");
        assert!(headers.if_modified_since.is_none());
    }

    #[test]
    fn build_serve_request_forwards_headers() {
        let cond = conditional(Some("bytes=0-99"), None, None);
        let req = build_serve_request(&cond);
        assert_eq!(req.headers().get(RANGE).unwrap(), "bytes=0-99");
        assert!(req.headers().get(IF_NONE_MATCH).is_none());
    }

    #[test]
    fn parses_every_single_range_form() {
        let cond = conditional(Some("bytes=0-99"), None, None);
        assert_eq!(
            parse_range(cond.range.as_ref()),
            Some(ByteRange::inclusive(0, 99))
        );

        let cond = conditional(Some("bytes=100-"), None, None);
        assert_eq!(
            parse_range(cond.range.as_ref()),
            Some(ByteRange::from_start(100))
        );

        let cond = conditional(Some("bytes=-500"), None, None);
        assert_eq!(
            parse_range(cond.range.as_ref()),
            Some(ByteRange::Suffix(500))
        );
    }

    #[test]
    fn an_unusable_range_header_is_ignored_rather_than_refused() {
        for value in ["items=0-9", "bytes=abc-def", "bytes=0-9,20-29", "bytes=9-4"] {
            let cond = conditional(Some(value), None, None);
            assert!(
                parse_range(cond.range.as_ref()).is_none(),
                "{value} must be ignored"
            );
        }

        assert!(parse_range(None).is_none());
    }

    #[test]
    fn a_backend_reported_entity_tag_wins_and_is_quoted_once() {
        let object = RangedObject::whole(b"abc".to_vec())
            .etag(Some("\"s3tag\"".to_string()))
            .build();

        assert_eq!(remote_etag("media/a.png", &object), "\"s3tag\"");
    }

    /// A backend that reports no tag still gets a strong one: stable for the
    /// same object, different for a different key or size.
    #[test]
    fn a_synthesised_entity_tag_is_stable_and_key_scoped() {
        let object = RangedObject::whole(b"abc".to_vec()).build();
        let other_key = remote_etag("media/b.png", &object);
        let tag = remote_etag("media/a.png", &object);

        assert_eq!(tag, remote_etag("media/a.png", &object));
        assert_ne!(tag, other_key);
        assert!(tag.starts_with('"') && tag.ends_with('"'), "{tag}");

        let bigger = RangedObject::whole(b"abcd".to_vec()).build();
        assert_ne!(tag, remote_etag("media/a.png", &bigger));
    }

    #[test]
    fn a_matching_entity_tag_is_fresh_and_a_different_one_is_not() {
        let cond = conditional(None, Some("\"abc\""), None);
        assert!(is_fresh(&cond, "\"abc\"", None));
        assert!(!is_fresh(&cond, "\"xyz\"", None));

        let weak = conditional(None, Some("W/\"abc\""), None);
        assert!(is_fresh(&weak, "\"abc\"", None));

        let any = conditional(None, Some("*"), None);
        assert!(is_fresh(&any, "\"abc\"", None));

        let list = conditional(None, Some("\"one\", \"abc\""), None);
        assert!(is_fresh(&list, "\"abc\"", None));
    }

    /// An entity tag decides alone: a stale `If-Modified-Since` cannot make a
    /// mismatched tag fresh.
    #[test]
    fn an_entity_tag_takes_precedence_over_the_date() {
        let date = "Sun, 06 Nov 1994 08:49:37 GMT";
        let cond = conditional(None, Some("\"xyz\""), Some(date));

        assert!(!is_fresh(&cond, "\"abc\"", Some(date)));
    }

    #[test]
    fn an_unmodified_date_is_fresh_and_a_newer_object_is_not() {
        let cond = conditional(None, None, Some("Sun, 06 Nov 1994 08:49:37 GMT"));

        assert!(is_fresh(
            &cond,
            "\"abc\"",
            Some("Sun, 06 Nov 1994 08:49:37 GMT")
        ));
        assert!(is_fresh(
            &cond,
            "\"abc\"",
            Some("Sat, 05 Nov 1994 08:49:37 GMT")
        ));
        assert!(!is_fresh(
            &cond,
            "\"abc\"",
            Some("Mon, 07 Nov 1994 08:49:37 GMT")
        ));
        assert!(!is_fresh(&cond, "\"abc\"", Some("not a date")));
        assert!(!is_fresh(&cond, "\"abc\"", None));
    }

    #[test]
    fn content_disposition_is_inline_for_images_and_attachment_otherwise() {
        assert_eq!(content_disposition("image/png", None), "inline");
        assert_eq!(content_disposition("image/svg+xml", None), "attachment");
        assert_eq!(content_disposition("application/pdf", None), "attachment");
    }

    /// Regression: a stored filename carrying control bytes (the extension is
    /// not sanitized upstream, so a crafted upload can smuggle a CRLF here) must
    /// not produce an unparseable header value.
    #[test]
    fn content_disposition_sanitizes_control_chars() {
        let disposition = content_disposition("application/pdf", Some("nano123456_photo.pd\r\nf"));
        assert_eq!(disposition, "attachment; filename=\"photo.pd__f\"");
        // Must be a valid header value (no panic on insert).
        assert!(disposition.parse::<HeaderValue>().is_ok());
    }

    /// A non-ASCII download name travels in RFC 6266 `filename*`, with an
    /// ASCII fallback; invisible bidi controls (U+202E can make `fdp.exe` read
    /// as `exe.pdf`) are replaced like control characters.
    #[test]
    fn content_disposition_encodes_unicode_and_neutralizes_bidi_controls() {
        let disposition = content_disposition(
            "application/pdf",
            Some("nano123456_Bericht über\u{202E}fdp.exe"),
        );

        assert_eq!(
            disposition,
            "attachment; filename=\"Bericht _ber_fdp.exe\"; filename*=UTF-8''Bericht%20%C3%BCber_fdp.exe"
        );
        assert!(disposition.parse::<HeaderValue>().is_ok());
    }

    /// Regression: the download name was cut at the FIRST underscore, so an
    /// id containing one (the nanoid alphabet has `_`) truncated the name.
    #[test]
    fn content_disposition_keeps_a_name_whose_id_contains_underscores() {
        let disposition = content_disposition("application/pdf", Some("ab_cd12_x9_report.pdf"));

        assert_eq!(disposition, "attachment; filename=\"report.pdf\"");
    }

    #[test]
    fn shared_headers_apply_to_any_response() {
        let mut response = Response::new(Body::empty());

        ServeHeaders::new("image/svg+xml", "private, no-store")
            .varied(true)
            .stored_name(Some("abcdefghij_logo.svg"))
            .apply(&mut response);

        let headers = response.headers();
        assert_eq!(headers.get(ACCEPT_RANGES).unwrap(), "bytes");
        assert_eq!(headers.get(CACHE_CONTROL).unwrap(), "private, no-store");
        assert_eq!(headers.get(VARY).unwrap(), "Accept");
        assert_eq!(
            headers.get(CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"logo.svg\""
        );
        assert_eq!(
            headers.get(CONTENT_SECURITY_POLICY).unwrap(),
            "sandbox; default-src 'none'"
        );
    }

    #[test]
    fn shared_headers_omit_vary_and_csp_when_not_applicable() {
        let mut response = Response::new(Body::empty());

        ServeHeaders::new("text/plain", "no-cache").apply(&mut response);

        let headers = response.headers();
        assert!(headers.get(VARY).is_none());
        assert!(headers.get(CONTENT_SECURITY_POLICY).is_none());
        assert_eq!(headers.get(CONTENT_DISPOSITION).unwrap(), "attachment");
    }
}
