//! The double-submit CSRF rule, in one place.
//!
//! The admin layout emits the token twice, because browsers submit in two
//! shapes: `layout/base.hbs` adds an `X-CSRF-Token` header to every htmx
//! request *and* a hidden `_csrf` field to every form. A surface that honours
//! only one of them rejects half of what the admin UI itself sends — which is
//! what custom routes did, so a plain `<form>` posting to a `csrf = true`
//! route always answered 403 while the identical form to a built-in admin
//! route was accepted.
//!
//! [`request_token_matches`] is that rule: the header first, then the `_csrf`
//! field of a urlencoded body, compared in constant time. Both the global
//! admin middleware and the custom-route dispatcher call it, so neither can
//! drift into accepting a different set of requests than the other.
//!
//! The external auth callback routes are the one exemption
//! ([`exempt_route`]): an identity provider's `response_mode=form_post`
//! arrives as a cross-site POST, which never carries the `SameSite=Strict`
//! token cookie. Login CSRF on a callback is what the OAuth `state` parameter
//! defends against — the callback hook must check it.

use axum::http::{HeaderMap, header::CONTENT_TYPE};
use subtle::ConstantTimeEq;

/// Header the admin layout sets on htmx requests.
pub(in crate::admin) const TOKEN_HEADER: &str = "x-csrf-token";

/// Hidden field the admin layout adds to every form submit.
pub(in crate::admin) const TOKEN_FIELD: &str = "_csrf";

/// Route of the un-scoped external auth callback.
pub(in crate::admin) const AUTH_CALLBACK_ROUTE: &str = "/admin/auth/callback/{name}";

/// Route of the collection-scoped external auth callback.
pub(in crate::admin) const AUTH_CALLBACK_SCOPED_ROUTE: &str =
    "/admin/auth/callback/{collection}/{name}";

/// Whether the route a request matched (its axum route template) is exempt
/// from the double-submit check: only the external auth callbacks, which an
/// identity provider may reach with a cross-site form POST. Their login-CSRF
/// defense is the OAuth `state` round trip their hook verifies.
pub(in crate::admin) fn exempt_route(matched: Option<&str>) -> bool {
    matched.is_some_and(|route| route == AUTH_CALLBACK_ROUTE || route == AUTH_CALLBACK_SCOPED_ROUTE)
}

/// Whether `candidate` is the expected token. Constant-time, and an empty
/// expectation never matches — an absent cookie must not authorise a request
/// that also sends nothing.
pub(in crate::admin) fn token_matches(expected: &str, candidate: &str) -> bool {
    if expected.is_empty() {
        return false;
    }

    candidate.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// The `X-CSRF-Token` header, when present and valid UTF-8.
pub(in crate::admin) fn header_token(headers: &HeaderMap) -> Option<&str> {
    headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok())
}

/// Whether the body is a urlencoded form, and so could carry `_csrf`.
pub(in crate::admin) fn is_form_urlencoded(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"))
}

/// The `_csrf` field of a urlencoded body.
pub(in crate::admin) fn form_token(body: &[u8]) -> Option<String> {
    form_urlencoded::parse(body)
        .find(|(k, _)| k == TOKEN_FIELD)
        .map(|(_, v)| v.to_string())
}

/// Whether the request carries a double-submit token matching `cookie`.
///
/// Checks the header first, then — only for a urlencoded body that the caller
/// has already buffered — the `_csrf` field. Pass `body: None` before reading
/// the body to settle the header case without buffering; a `false` there means
/// "not yet", not "rejected", whenever [`is_form_urlencoded`] also holds.
pub(in crate::admin) fn request_token_matches(
    cookie: &str,
    headers: &HeaderMap,
    body: Option<&[u8]>,
) -> bool {
    if let Some(header) = header_token(headers)
        && token_matches(cookie, header)
    {
        return true;
    }

    let Some(bytes) = body.filter(|_| is_form_urlencoded(headers)) else {
        return false;
    };

    form_token(bytes).is_some_and(|token| token_matches(cookie, &token))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the two auth callback routes skip the check — not a sibling
    /// route, not an unmatched request.
    #[test]
    fn only_the_auth_callbacks_are_exempt() {
        assert!(exempt_route(Some("/admin/auth/callback/{name}")));
        assert!(exempt_route(Some(
            "/admin/auth/callback/{collection}/{name}"
        )));

        assert!(!exempt_route(Some("/admin/mfa")));
        assert!(!exempt_route(Some("/admin/login")));
        assert!(!exempt_route(Some("/admin/collections/{slug}")));
        assert!(!exempt_route(None));
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(*k, v.parse().unwrap());
        }
        map
    }

    fn form_headers() -> HeaderMap {
        headers(&[("content-type", "application/x-www-form-urlencoded")])
    }

    #[test]
    fn a_matching_header_is_accepted_without_a_body() {
        let h = headers(&[("x-csrf-token", "tok")]);

        assert!(request_token_matches("tok", &h, None));
    }

    /// The gap this module closes: the admin layout puts the token in a form
    /// field, and a surface that reads only the header rejected every plain
    /// form submit.
    #[test]
    fn a_matching_form_field_is_accepted() {
        assert!(request_token_matches(
            "tok",
            &form_headers(),
            Some(b"slug=weekly&_csrf=tok")
        ));
    }

    /// The header is checked first, so a request carrying both is accepted on
    /// the header alone and never has to be buffered.
    #[test]
    fn the_header_is_consulted_before_the_body() {
        let mut h = form_headers();
        h.insert("x-csrf-token", "tok".parse().unwrap());

        assert!(request_token_matches("tok", &h, None));
    }

    #[test]
    fn a_mismatched_token_is_refused_in_either_position() {
        let h = headers(&[("x-csrf-token", "other")]);
        assert!(!request_token_matches("tok", &h, None));

        assert!(!request_token_matches(
            "tok",
            &form_headers(),
            Some(b"_csrf=other")
        ));
    }

    /// An absent cookie must not authorise a request that also sends nothing —
    /// otherwise "" == "" would pass.
    #[test]
    fn an_empty_expectation_never_matches() {
        assert!(!token_matches("", ""));

        let h = headers(&[("x-csrf-token", "")]);
        assert!(!request_token_matches("", &h, None));
    }

    /// The field is only read from a urlencoded body: a JSON body that happens
    /// to contain the bytes `_csrf=…` is not a form submit.
    #[test]
    fn the_field_is_only_read_from_a_urlencoded_body() {
        let json = headers(&[("content-type", "application/json")]);

        assert!(!request_token_matches(
            "tok",
            &json,
            Some(b"{\"_csrf\":\"tok\"}")
        ));
    }

    /// Before the body is read there is nothing to fall back to, so a form
    /// submit is undecided rather than accepted.
    #[test]
    fn a_form_submit_is_undecided_until_its_body_is_read() {
        assert!(!request_token_matches("tok", &form_headers(), None));
        assert!(is_form_urlencoded(&form_headers()));
    }
}
