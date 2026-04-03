//! Session cookie helpers for auth handlers.

use axum::{http::header::SET_COOKIE, response::Response};

/// MFA pending cookie expiry in seconds (5 minutes).
const MFA_PENDING_EXPIRY: u64 = 300;

/// A built `Set-Cookie` header value.
pub(in crate::admin::handlers) struct Cookie {
    value: String,
}

impl Cookie {
    /// Start building a cookie with the given name and value.
    pub fn builder<'a>(name: &'a str, value: &'a str) -> CookieBuilder<'a> {
        CookieBuilder {
            name,
            value,
            max_age: 0,
            http_only: true,
        }
    }

    /// Return the `Set-Cookie` header value.
    #[cfg(test)]
    pub fn header(&self) -> &str {
        &self.value
    }
}

impl std::fmt::Display for Cookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.value)
    }
}

/// Builder for `Set-Cookie` header values with consistent flags.
pub(in crate::admin::handlers) struct CookieBuilder<'a> {
    name: &'a str,
    value: &'a str,
    max_age: u64,
    http_only: bool,
}

impl<'a> CookieBuilder<'a> {
    pub fn max_age(mut self, seconds: u64) -> Self {
        self.max_age = seconds;
        self
    }

    pub fn http_only(mut self, enabled: bool) -> Self {
        self.http_only = enabled;
        self
    }

    /// Build the `Set-Cookie` header value.
    /// `dev_mode` controls whether the `Secure` flag is set.
    pub fn build(self, dev_mode: bool) -> Cookie {
        let http_only = if self.http_only { "; HttpOnly" } else { "" };
        let secure = if dev_mode { "" } else { "; Secure" };

        Cookie {
            value: format!(
                "{}={}{}; Path=/; SameSite=Lax; Max-Age={}{}",
                self.name, self.value, http_only, self.max_age, secure,
            ),
        }
    }
}

/// Build `Set-Cookie` header values for the session.
pub(in crate::admin::handlers) fn session_cookies(
    token: &str,
    expiry: u64,
    exp: u64,
    dev_mode: bool,
) -> Vec<String> {
    vec![
        Cookie::builder("crap_session", token)
            .max_age(expiry)
            .build(dev_mode)
            .to_string(),
        Cookie::builder("crap_session_exp", &exp.to_string())
            .max_age(expiry)
            .http_only(false)
            .build(dev_mode)
            .to_string(),
    ]
}

/// Build a `Set-Cookie` header value for the MFA pending token.
pub(in crate::admin::handlers) fn mfa_pending_cookie(token: &str, dev_mode: bool) -> String {
    Cookie::builder("crap_mfa_pending", token)
        .max_age(MFA_PENDING_EXPIRY)
        .build(dev_mode)
        .to_string()
}

/// Build a `Set-Cookie` header value that clears the MFA pending cookie.
pub(in crate::admin::handlers) fn clear_mfa_pending_cookie(dev_mode: bool) -> String {
    Cookie::builder("crap_mfa_pending", "")
        .build(dev_mode)
        .to_string()
}

/// Append `Set-Cookie` headers to an existing response.
pub(in crate::admin::handlers) fn append_cookies(response: &mut Response, cookies: &[String]) {
    for cookie in cookies {
        response.headers_mut().append(
            SET_COOKIE,
            cookie.parse().expect("cookie header is valid ASCII"),
        );
    }
}

/// Build `Set-Cookie` header values that clear both session cookies.
pub(in crate::admin::handlers) fn clear_session_cookies(dev_mode: bool) -> Vec<String> {
    vec![
        Cookie::builder("crap_session", "")
            .build(dev_mode)
            .to_string(),
        Cookie::builder("crap_session_exp", "")
            .http_only(false)
            .build(dev_mode)
            .to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookies_dev_mode() {
        let cookies = session_cookies("tok123", 7200, 1700000000, true);
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].contains("crap_session=tok123"));
        assert!(cookies[0].contains("HttpOnly"));
        assert!(cookies[0].contains("Max-Age=7200"));
        assert!(!cookies[0].contains("Secure"));
        assert!(cookies[1].contains("crap_session_exp=1700000000"));
        assert!(!cookies[1].contains("HttpOnly"));
        assert!(cookies[1].contains("Max-Age=7200"));
        assert!(!cookies[1].contains("Secure"));
    }

    #[test]
    fn session_cookies_production_mode() {
        let cookies = session_cookies("tok456", 3600, 1700003600, false);
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].contains("crap_session=tok456"));
        assert!(cookies[0].contains("Max-Age=3600"));
        assert!(cookies[0].contains("; Secure"));
        assert!(cookies[1].contains("crap_session_exp=1700003600"));
        assert!(!cookies[1].contains("HttpOnly"));
        assert!(cookies[1].contains("; Secure"));
    }

    #[test]
    fn clear_session_cookies_dev_mode() {
        let cookies = clear_session_cookies(true);
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].contains("crap_session=;"));
        assert!(cookies[0].contains("Max-Age=0"));
        assert!(!cookies[0].contains("Secure"));
        assert!(cookies[1].contains("crap_session_exp=;"));
        assert!(cookies[1].contains("Max-Age=0"));
        assert!(!cookies[1].contains("HttpOnly"));
    }

    #[test]
    fn clear_session_cookies_production_mode() {
        let cookies = clear_session_cookies(false);
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].contains("crap_session=;"));
        assert!(cookies[0].contains("Max-Age=0"));
        assert!(cookies[0].contains("; Secure"));
        assert!(cookies[1].contains("crap_session_exp=;"));
        assert!(cookies[1].contains("; Secure"));
    }

    #[test]
    fn mfa_pending_cookie_dev_mode() {
        let cookie = mfa_pending_cookie("mfa-tok", true);
        assert!(cookie.contains("crap_mfa_pending=mfa-tok"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Max-Age=300"));
        assert!(!cookie.contains("Secure"));
    }

    #[test]
    fn mfa_pending_cookie_production_mode() {
        let cookie = mfa_pending_cookie("mfa-tok", false);
        assert!(cookie.contains("crap_mfa_pending=mfa-tok"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Max-Age=300"));
        assert!(cookie.contains("; Secure"));
    }

    #[test]
    fn clear_mfa_pending_cookie_dev_mode() {
        let cookie = clear_mfa_pending_cookie(true);
        assert!(cookie.contains("crap_mfa_pending=;"));
        assert!(cookie.contains("Max-Age=0"));
        assert!(!cookie.contains("Secure"));
    }

    #[test]
    fn clear_mfa_pending_cookie_production_mode() {
        let cookie = clear_mfa_pending_cookie(false);
        assert!(cookie.contains("crap_mfa_pending=;"));
        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.contains("; Secure"));
    }

    #[test]
    fn builder_defaults_to_http_only() {
        let c = Cookie::builder("test", "val").max_age(60).build(true);
        assert!(c.header().contains("HttpOnly"));
    }

    #[test]
    fn builder_visible_removes_http_only() {
        let c = Cookie::builder("test", "val")
            .max_age(60)
            .http_only(false)
            .build(true);
        assert!(!c.header().contains("HttpOnly"));
    }

    #[test]
    fn builder_display_matches_header() {
        let c = Cookie::builder("x", "y").max_age(1).build(true);
        assert_eq!(c.to_string(), c.header());
    }
}
