//! The locale a write operation targets.

use crate::{
    core::{FieldError, ValidationError},
    db::{LocaleContext, LocaleMode},
    service::ServiceError,
};

/// Refuse the all-locales mode on a write — the ONE place that rule lives.
///
/// `locale = "all"` shapes a READ: every localized field comes back as a
/// `{ en = .., de = .. }` map. A write has no such shape. It used to be
/// accepted and then silently resolve to the DEFAULT locale's columns —
/// writing a locale the caller did not name — while also skipping the
/// shared-field lock that a non-default-locale write obeys, so a shared
/// field could be clobbered along the way.
///
/// Every operation body hands its decoded locale to [`write_locale_ctx`], and
/// the upload service — whose file-bearing writes reach `create_document` /
/// `update_document` without going through an operation — calls this directly,
/// so the admin form, Lua, gRPC, MCP and a multipart upload are all refused
/// alike. It is refused the way an unconfigured locale code is: a per-field
/// validation failure naming `locale`, which each surface already maps onto its
/// 400-class status.
///
/// # Errors
///
/// Returns a `locale` validation error when the context is in the all-locales
/// mode.
pub(crate) fn reject_all_locales(locale_ctx: Option<&LocaleContext>) -> Result<(), ServiceError> {
    let Some(ctx) = locale_ctx.filter(|ctx| matches!(ctx.mode, LocaleMode::All)) else {
        return Ok(());
    };

    Err(ServiceError::Validation(ValidationError::new(vec![
        FieldError::new(
            "locale",
            format!(
                "Invalid locale 'all' for a write — a write targets one locale. \
                 Available locales: {}",
                ctx.config.locales.join(", ")
            ),
        ),
    ])))
}

/// The locale context a write operation runs under, rejecting the all-locales
/// mode through [`reject_all_locales`].
pub(super) fn write_locale_ctx(
    locale_ctx: Option<LocaleContext>,
) -> Result<Option<LocaleContext>, ServiceError> {
    reject_all_locales(locale_ctx.as_ref())?;

    Ok(locale_ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;

    fn en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn ctx(mode: LocaleMode) -> LocaleContext {
        LocaleContext {
            mode,
            config: en_de(),
        }
    }

    /// `all` is a read shape. Accepting it on a write wrote the default locale
    /// under another name and skipped the shared-field lock.
    #[test]
    fn a_write_refuses_the_all_locales_mode() {
        let err = write_locale_ctx(Some(ctx(LocaleMode::All))).unwrap_err();

        let ServiceError::Validation(validation) = err else {
            panic!("expected a validation error, got {err:?}");
        };
        let fields = validation.to_field_map();
        let message = fields.get("locale").expect("the locale field is named");
        assert!(message.contains("'all'"), "unexpected: {message}");
        assert!(message.contains("en, de"), "unexpected: {message}");
    }

    /// Every other mode passes through untouched, including no context at all
    /// (localization disabled).
    #[test]
    fn a_single_or_default_locale_passes_through() {
        assert!(write_locale_ctx(None).unwrap().is_none());

        for mode in [LocaleMode::Default, LocaleMode::Single("de".to_string())] {
            let resolved = write_locale_ctx(Some(ctx(mode))).unwrap().expect("kept");
            assert!(!matches!(resolved.mode, LocaleMode::All));
        }
    }
}
