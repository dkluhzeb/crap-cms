//! Locale helpers — editor locale extraction and template data building.

use axum::http::{HeaderMap, header};
use tracing::warn;

use crate::{
    admin::{
        AdminState, context::LocaleTemplateData, handlers::auth::EDITOR_LOCALE_COOKIE,
        server::extract_cookie,
    },
    config::LocaleConfig,
    core::{DocumentFields, FieldDefinition, flatten_group_fields},
    db::{LocaleContext, query::locale_locked_field_names},
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

    let raw = extract_cookie(cookie_str, EDITOR_LOCALE_COOKIE);
    let locale = raw.unwrap_or(&config.default_locale);

    if config.locales.contains(&locale.to_string()) {
        Some(locale.to_string())
    } else {
        Some(config.default_locale.clone())
    }
}

/// Parse an explicitly requested locale (form `_locale` field / validate
/// payload), rejecting unknown locales. Swallowing the parse error would
/// drop the locale context entirely, and a `None` context on a localized
/// collection reads/writes bare columns (`title` vs `title__en`) — so a bad
/// locale string must 400 at the surface, matching the gRPC/MCP/Lua APIs.
pub fn parse_request_locale(
    locale: Option<&str>,
    config: &LocaleConfig,
) -> Result<Option<LocaleContext>, String> {
    LocaleContext::from_locale_string(locale, config).map_err(|e| e.to_string())
}

/// Build locale template context (selector data) from config + current locale.
/// Returns `(locale_ctx_for_db, locale_template_data)` — the second element
/// is `None` when locale support is disabled, otherwise carries the typed
/// picker data the page contexts flatten into themselves.
pub fn build_locale_template_data(
    state: &AdminState,
    requested_locale: Option<&str>,
) -> (Option<LocaleContext>, Option<LocaleTemplateData>) {
    let config = &state.config.locale;

    let locale_ctx = if config.is_enabled() {
        let current = requested_locale.unwrap_or(&config.default_locale);
        LocaleContext::from_locale_string(Some(current), config)
            .inspect_err(|e| {
                warn!("Invalid editor locale '{current}' — falling back to no locale context: {e}");
            })
            .unwrap_or(None)
    } else {
        None
    };

    let template_data = LocaleTemplateData::for_locale(config, requested_locale);

    (locale_ctx, template_data)
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

/// Strip shared (locale-locked) fields from a non-default-locale PUBLISH
/// (collections and globals alike).
///
/// The admin edit form submits shared (non-localized) fields as read-only
/// display artifacts — including the hidden row-id / `_block_type` inputs of a
/// shared array/blocks field. Under a non-default locale the service rejects a
/// write that carries them, to stop a programmatic caller (gRPC/Lua/MCP) from
/// silently overwriting the canonical default-locale value. `save_draft` already
/// drops them on the draft path; this does the same on publish so saving a
/// translation isn't rejected, while the service guard still protects the
/// programmatic surfaces. No-op for the default locale, drafts, or when nothing
/// is locale-locked.
pub(crate) fn strip_locale_locked_for_publish(
    data: DocumentFields,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
    draft: bool,
) -> DocumentFields {
    if draft {
        return data;
    }

    let locked = locale_locked_field_names(fields, locale_ctx);
    if locked.is_empty() {
        return data;
    }

    // Flatten groups to `group__sub` so the locked-name filter matches the shape
    // `locale_locked_field_names` produces; the write path re-nests.
    flatten_group_fields(&data, fields)
        .into_iter()
        .filter(|(k, _)| !locked.contains(k))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::FieldType;
    use crate::db::query::LocaleMode;

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

    // ── publish strip ──────────────────────────────────────────────────────

    fn shared_fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("slug", FieldType::Text).build(),
            FieldDefinition::builder("tags", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("name", FieldType::Text).build(),
                ])
                .build(),
        ]
    }

    fn locked_ctx(locale: &str) -> LocaleContext {
        LocaleContext {
            mode: LocaleMode::Single(locale.to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        }
    }

    /// Regression: publishing a non-default-locale edit strips the shared
    /// (locale-locked) fields the admin form submits read-only — including a
    /// shared array whose only submitted key is the new hidden row id — so the
    /// service's shared-field guard doesn't reject the whole save. The localized
    /// field is kept.
    #[test]
    fn publish_strips_shared_fields_under_non_default_locale() {
        let fields = shared_fields();
        let data: DocumentFields = [
            ("title".to_string(), json!("Titel")),
            ("slug".to_string(), json!("neu")),
            ("tags".to_string(), json!([{ "id": "row1", "name": "x" }])),
        ]
        .into_iter()
        .collect();

        let out = strip_locale_locked_for_publish(data, &fields, Some(&locked_ctx("de")), false);
        assert!(out.contains_key("title"), "localized field is kept");
        assert!(!out.contains_key("slug"), "shared scalar is stripped");
        assert!(
            !out.contains_key("tags"),
            "shared array (hidden-id-only submit) is stripped, so publish isn't rejected"
        );
    }

    /// No-op off the non-default-locale publish path: default locale, drafts
    /// (`save_draft` strips), and a `None` locale context all pass data through.
    #[test]
    fn publish_strip_is_noop_off_the_non_default_publish_path() {
        let fields = shared_fields();
        let shared =
            || -> DocumentFields { [("slug".to_string(), json!("neu"))].into_iter().collect() };

        assert!(
            strip_locale_locked_for_publish(shared(), &fields, Some(&locked_ctx("en")), false)
                .contains_key("slug"),
            "default locale is untouched"
        );
        assert!(
            strip_locale_locked_for_publish(shared(), &fields, Some(&locked_ctx("de")), true)
                .contains_key("slug"),
            "draft path is untouched (save_draft strips)"
        );
        assert!(
            strip_locale_locked_for_publish(shared(), &fields, None, false).contains_key("slug"),
            "no locale context is untouched"
        );
    }
}
