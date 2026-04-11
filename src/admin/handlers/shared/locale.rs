//! Locale helpers — editor locale extraction and template data building.

use axum::http::{HeaderMap, header};
use serde_json::{Value, json};

use crate::{
    admin::{AdminState, server::extract_cookie},
    config::LocaleConfig,
    db::LocaleContext,
};

/// Extract the editor locale from the `crap_editor_locale` cookie.
/// Falls back to the config's default locale if the cookie is absent or invalid.
/// Returns `None` if locales are not enabled.
pub fn extract_editor_locale(headers: &HeaderMap, config: &LocaleConfig) -> Option<String> {
    if !config.is_enabled() {
        return None;
    }

    let cookie_str = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let raw = extract_cookie(cookie_str, "crap_editor_locale");
    let locale = raw.unwrap_or(&config.default_locale);

    if config.locales.contains(&locale.to_string()) {
        Some(locale.to_string())
    } else {
        Some(config.default_locale.clone())
    }
}

/// Build locale template context (selector data) from config + current locale.
/// Returns `(locale_ctx_for_db, template_json)` where template_json has
/// `has_locales`, `current_locale`, `locales` (array with value/label/selected).
pub fn build_locale_template_data(
    state: &AdminState,
    requested_locale: Option<&str>,
) -> (Option<LocaleContext>, Value) {
    let config = &state.config.locale;

    if !config.is_enabled() {
        return (None, json!({}));
    }

    let current = requested_locale.unwrap_or(&config.default_locale);
    let locale_ctx = LocaleContext::from_locale_string(Some(current), config).unwrap_or(None);

    let locales: Vec<Value> = config
        .locales
        .iter()
        .map(|l| {
            json!({
                "value": l,
                "label": l.to_uppercase(),
                "selected": l == current,
            })
        })
        .collect();

    let data = json!({
        "has_locales": true,
        "current_locale": current,
        "locales": locales,
    });

    (locale_ctx, data)
}

/// Check if the current locale is a non-default locale (fields should be locked).
pub fn is_non_default_locale(state: &AdminState, requested_locale: Option<&str>) -> bool {
    let config = &state.config.locale;

    if !config.is_enabled() {
        return false;
    }

    let current = requested_locale.unwrap_or(&config.default_locale);
    current != config.default_locale
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locale_config_enabled() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string(), "fr".to_string()],
            fallback: false,
        }
    }

    #[test]
    fn extract_editor_locale_from_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "crap_editor_locale=de".parse().unwrap());
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("de".to_string()));
    }

    #[test]
    fn extract_editor_locale_falls_back_to_default() {
        let headers = HeaderMap::new();
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("en".to_string()));
    }

    #[test]
    fn extract_editor_locale_invalid_locale_falls_back() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "crap_editor_locale=zz".parse().unwrap());
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("en".to_string()));
    }

    #[test]
    fn extract_editor_locale_disabled_returns_none() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "crap_editor_locale=de".parse().unwrap());
        let config = LocaleConfig::default();
        let result = extract_editor_locale(&headers, &config);
        assert_eq!(result, None);
    }

    #[test]
    fn extract_editor_locale_with_multiple_cookies() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "crap_session=abc; crap_editor_locale=fr; other=xyz"
                .parse()
                .unwrap(),
        );
        let result = extract_editor_locale(&headers, &locale_config_enabled());
        assert_eq!(result, Some("fr".to_string()));
    }
}
