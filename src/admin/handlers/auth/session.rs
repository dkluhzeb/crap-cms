//! Session cookie helpers for auth handlers.

/// Build `Set-Cookie` header values for the session.
pub(in crate::admin::handlers) fn session_cookies(
    token: &str,
    expiry: u64,
    exp: u64,
    dev_mode: bool,
) -> Vec<String> {
    let secure = if dev_mode { "" } else { "; Secure" };

    vec![
        format!(
            "crap_session={}; HttpOnly; Path=/; SameSite=Lax; Max-Age={}{}",
            token, expiry, secure,
        ),
        format!(
            "crap_session_exp={}; Path=/; SameSite=Lax; Max-Age={}{}",
            exp, expiry, secure,
        ),
    ]
}

/// Build `Set-Cookie` header values that clear both session cookies.
pub(in crate::admin::handlers) fn clear_session_cookies(dev_mode: bool) -> Vec<String> {
    let secure = if dev_mode { "" } else { "; Secure" };

    vec![
        format!(
            "crap_session=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0{}",
            secure
        ),
        format!(
            "crap_session_exp=; Path=/; SameSite=Lax; Max-Age=0{}",
            secure
        ),
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
}
