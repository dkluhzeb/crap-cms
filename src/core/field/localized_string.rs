//! A string that can be plain or per-locale, and the locale it resolves in.
//!
//! Operator-authored labels (`admin.label`, `labels.plural`, select option
//! labels, block labels, placeholders, descriptions) may be a per-locale map.
//! [`LocalizedString::resolve_current`] resolves one for the **active label
//! locale**: the viewer's admin UI locale while a request scope is entered
//! ([`with_label_locale`]), then the configured `locale.default_locale`
//! (installed once at startup via [`set_default_label_locale`]), then — so a
//! label never renders blank while it has any translation — the
//! alphabetically-first key.
//!
//! The request scope is a `tokio` task-local (same approach as the CSP nonce):
//! the admin middleware enters it once per request, so every label resolved
//! while building that request's page follows the viewer's UI locale without
//! threading it through every context builder. Work moved onto a
//! `spawn_blocking` thread carries it over with [`in_label_locale`].

use std::{collections::HashMap, future::Future, sync::OnceLock};

use serde::{Deserialize, Serialize};
use tokio::{
    task::{JoinHandle, spawn_blocking},
    task_local,
};

use crate::typegen::lua::LuaAlias;

/// The configured `locale.default_locale`, installed once when the config is
/// applied. Unset (unit tests, tooling that never applies a config) means
/// there is no default to prefer.
static DEFAULT_LABEL_LOCALE: OnceLock<String> = OnceLock::new();

task_local! {
    /// The viewer's admin UI locale for the request in progress.
    static LABEL_LOCALE: String;
}

/// Install the configured default locale process-wide. The first install
/// wins; the value is fixed for the process lifetime.
pub fn set_default_label_locale(locale: &str) {
    let _ = DEFAULT_LABEL_LOCALE.set(locale.to_string());
}

/// The configured default locale — the admin UI language of a viewer who has
/// not chosen one. `"en"` before a config is applied (unit tests, tooling).
#[must_use]
pub fn default_label_locale() -> &'static str {
    DEFAULT_LABEL_LOCALE.get().map_or("en", String::as_str)
}

/// Run `fut` with `ui_locale` as the active label locale.
pub async fn with_label_locale<F: Future>(ui_locale: String, fut: F) -> F::Output {
    LABEL_LOCALE.scope(ui_locale, fut).await
}

/// The active request's label locale, if a scope is entered on this task.
/// Capture it before moving work onto a `spawn_blocking` thread and hand it to
/// [`in_label_locale`] there.
#[must_use]
pub fn current_label_locale() -> Option<String> {
    LABEL_LOCALE.try_with(Clone::clone).ok()
}

/// Run `f` synchronously with `ui_locale` (when `Some`) as the active label
/// locale — the `spawn_blocking` counterpart of [`with_label_locale`].
#[must_use]
pub fn in_label_locale<R>(ui_locale: Option<String>, f: impl FnOnce() -> R) -> R {
    match ui_locale {
        Some(locale) => LABEL_LOCALE.sync_scope(locale, f),
        None => f(),
    }
}

/// Run `f` on a blocking thread inside the caller's active label locale — the
/// one way request work moves onto `spawn_blocking`, so a label a hook or a
/// template resolves there follows the viewer's UI locale like one resolved on
/// the request task.
pub fn spawn_blocking_in_label_locale<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let label_locale = current_label_locale();

    spawn_blocking(move || in_label_locale(label_locale, f))
}

/// A string that can be plain or per-locale.
/// Plain: `"Title"` — used as-is.
/// Localized: `{ en = "Title", de = "Titel" }` — resolved based on admin locale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, LuaAlias)]
#[serde(untagged)]
#[lua(alias = "crap.LocalizedString")]
pub enum LocalizedString {
    /// A simple, non-localized string.
    Plain(String),
    /// A map of locale identifiers to their localized strings.
    Localized(HashMap<String, String>),
}

impl LocalizedString {
    /// Resolve to a single string for the given locale, falling back to
    /// `default_locale`, then to the alphabetically-first key.
    #[must_use]
    pub fn resolve(&self, locale: &str, default_locale: &str) -> &str {
        let LocalizedString::Localized(map) = self else {
            return self.first_available();
        };

        map.get(locale)
            .or_else(|| map.get(default_locale))
            .map_or_else(|| self.first_available(), String::as_str)
    }

    /// Resolve for the active label locale: the request's UI locale when a
    /// scope is entered, then the installed default locale, then the
    /// alphabetically-first key. Outside any request this is the default
    /// locale's value.
    #[must_use]
    pub fn resolve_current(&self) -> &str {
        let default = DEFAULT_LABEL_LOCALE.get().map_or("", String::as_str);

        match current_label_locale() {
            Some(ui) => self.resolve(&ui, default),
            None => self.resolve(default, default),
        }
    }

    /// The plain string, or the alphabetically-first key's value — the
    /// deterministic last resort when no requested locale matches.
    fn first_available(&self) -> &str {
        match self {
            LocalizedString::Plain(s) => s,
            LocalizedString::Localized(map) => map
                .keys()
                .min()
                .and_then(|k| map.get(k))
                .map_or("", String::as_str),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn localized(pairs: &[(&str, &str)]) -> LocalizedString {
        LocalizedString::Localized(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        )
    }

    #[test]
    fn localized_string_resolve_existing_locale() {
        let ls = localized(&[("en", "Title"), ("de", "Titel")]);
        assert_eq!(ls.resolve("de", "en"), "Titel");
    }

    #[test]
    fn localized_string_resolve_fallback_to_default() {
        let ls = localized(&[("en", "Title")]);
        assert_eq!(ls.resolve("fr", "en"), "Title");
    }

    #[test]
    fn localized_string_resolve_plain() {
        let ls = LocalizedString::Plain("Hello".to_string());
        assert_eq!(ls.resolve_current(), "Hello");
        assert_eq!(ls.resolve("de", "en"), "Hello");
    }

    #[test]
    fn localized_string_resolve_empty() {
        let ls = LocalizedString::Localized(HashMap::new());
        assert_eq!(ls.resolve("en", "en"), "");
        assert_eq!(ls.resolve_current(), "");
    }

    /// Neither the requested nor the default locale is translated: the
    /// alphabetically-first key is used rather than rendering blank.
    #[test]
    fn resolve_falls_back_to_first_key_when_nothing_matches() {
        let ls = localized(&[("fr", "Titre"), ("de", "Titel")]);
        assert_eq!(ls.resolve("en", "en"), "Titel");
    }

    /// Regression: labels resolved to the alphabetically-first key (`de`) for
    /// every viewer. Inside a label-locale scope the viewer's UI locale wins.
    #[tokio::test]
    async fn resolve_current_follows_the_request_ui_locale() {
        let ls = localized(&[("de", "Titel"), ("en", "Title")]);

        let en = with_label_locale("en".to_string(), async { ls.resolve_current().to_string() });
        assert_eq!(en.await, "Title");

        let de = with_label_locale("de".to_string(), async { ls.resolve_current().to_string() });
        assert_eq!(de.await, "Titel");
    }

    /// Regression: work moved onto a blocking thread (every admin write, the
    /// edit page's read, restore, empty trash) resolved labels against the
    /// default locale instead of the viewer's UI locale.
    #[tokio::test]
    async fn blocking_work_keeps_the_caller_label_locale() {
        let ls = localized(&[("de", "Titel"), ("en", "Title"), ("fr", "Titre")]);

        let resolved = with_label_locale("fr".to_string(), async move {
            spawn_blocking_in_label_locale(move || ls.resolve_current().to_string()).await
        });

        assert_eq!(resolved.await.unwrap(), "Titre");
    }

    /// Outside any label-locale scope the blocking work runs unscoped.
    #[tokio::test]
    async fn blocking_work_without_a_scope_stays_unscoped() {
        let scoped = spawn_blocking_in_label_locale(current_label_locale).await;

        assert_eq!(scoped.unwrap(), None);
    }

    /// A UI locale the label does not translate falls back to the default
    /// locale (or, with none installed, the first key) — never blank.
    #[tokio::test]
    async fn resolve_current_untranslated_ui_locale_is_not_blank() {
        let ls = localized(&[("de", "Titel"), ("en", "Title")]);

        let fr = with_label_locale("fr".to_string(), async { ls.resolve_current().to_string() });
        assert!(!fr.await.is_empty());
    }

    /// The scope carries onto a blocking thread via `in_label_locale`.
    #[tokio::test]
    async fn label_locale_propagates_into_blocking_work() {
        let ls = localized(&[("de", "Titel"), ("en", "Title")]);

        let resolved = with_label_locale("en".to_string(), async move {
            let captured = current_label_locale();

            tokio::task::spawn_blocking(move || {
                in_label_locale(captured, || ls.resolve_current().to_string())
            })
            .await
            .unwrap()
        })
        .await;

        assert_eq!(resolved, "Title");
    }

    #[test]
    fn no_label_locale_outside_a_scope() {
        assert_eq!(current_label_locale(), None);
    }
}
