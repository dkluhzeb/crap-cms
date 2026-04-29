//! Base page-context structs flattened into every per-page typed context.
//!
//! [`BasePageContext`] is the full base — present on every authenticated
//! admin page (dashboard, collection list/edit, globals, etc.). It carries
//! the navigation tree, current user, locale data, and page metadata.
//!
//! [`AuthBasePageContext`] is the minimal base for unauthenticated pages
//! (login, password reset, MFA). It omits navigation and user fields.
//!
//! Per-page structs flatten one of these via `#[serde(flatten)]` and add
//! their page-specific fields on top.

use axum::Extension;
use schemars::JsonSchema;
use serde::Serialize;

use super::{Breadcrumb, PageMeta};
use crate::{
    admin::{
        AdminState,
        context::{CrapMeta, EditorLocaleContext, EditorLocaleOption, NavData, UserContext},
        handlers::shared::has_read_access,
    },
    core::auth::{AuthUser, Claims},
};

/// Common fields present on every authenticated admin page.
#[derive(Serialize, JsonSchema)]
pub struct BasePageContext {
    pub crap: CrapMeta,
    pub nav: NavData,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<UserContext>,

    /// Active UI translation locale.
    #[serde(rename = "_locale")]
    pub locale: String,

    /// Available UI translation locales (for the locale picker).
    pub available_locales: Vec<String>,

    /// Page title — duplicated at top level for backward compat with the
    /// base layout that reads `{{title}}` directly. Templates that have
    /// migrated read `{{page.title}}` instead.
    pub title: String,

    pub page: PageMeta,

    /// Top-level breadcrumb mirror of `page.breadcrumbs`. The breadcrumb
    /// partial prefers `page.breadcrumbs` and falls back to this. Kept for
    /// backward compat with overridden templates.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub breadcrumbs: Vec<Breadcrumb>,

    // ── Editor (content) locale fields — present only when content-locale
    //    support is enabled in the config. Flattened to top level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_editor_locales: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_locale: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_locales: Option<Vec<EditorLocaleOption>>,
}

/// Minimal base for unauthenticated pages (login / forgot / reset / MFA).
/// Omits `nav` and `user`.
#[derive(Serialize, JsonSchema)]
pub struct AuthBasePageContext {
    pub crap: CrapMeta,

    #[serde(rename = "_locale")]
    pub locale: String,

    pub available_locales: Vec<String>,

    pub title: String,

    pub page: PageMeta,
}

impl BasePageContext {
    /// Construct a base context for an authenticated handler. Encapsulates
    /// the per-page boilerplate (active UI locale, nav, user, request-scoped
    /// CSP nonce, available translation locales).
    ///
    /// `auth_user` is the optional axum extension carried by every handler
    /// (None means the request didn't go through auth middleware — only
    /// possible for error pages).
    pub fn for_handler(
        state: &AdminState,
        claims: Option<&Claims>,
        auth_user: &Option<Extension<AuthUser>>,
        page: PageMeta,
    ) -> Self {
        let user = claims.map(UserContext::from_claims);
        let locale = auth_user
            .as_ref()
            .map(|Extension(au)| au.ui_locale.clone())
            .unwrap_or_else(|| state.config.locale.default_locale.clone());
        let user_doc = auth_user.as_ref().map(|Extension(au)| &au.user_doc);

        let mut nav = NavData::from_state(state);
        filter_nav_in_place(&mut nav, state, user_doc);

        let title = page.title.clone();
        let available_locales = state
            .translations
            .available_locales()
            .into_iter()
            .map(str::to_string)
            .collect();

        Self {
            crap: CrapMeta::from_state(state),
            nav,
            user,
            locale,
            available_locales,
            title,
            page,
            breadcrumbs: Vec::new(),
            has_editor_locales: None,
            editor_locale: None,
            editor_locales: None,
        }
    }

    /// Attach a breadcrumb trail (writes both `page.breadcrumbs` and the
    /// top-level mirror).
    pub fn with_breadcrumbs(mut self, breadcrumbs: Vec<Breadcrumb>) -> Self {
        self.breadcrumbs = breadcrumbs.clone();
        self.page.breadcrumbs = breadcrumbs;
        self
    }

    /// Attach editor (content) locale data — no-op when content locales are
    /// disabled in the config.
    pub fn with_editor_locale(mut self, editor_locale: Option<&str>, state: &AdminState) -> Self {
        if let Some(ctx) = EditorLocaleContext::for_locale(editor_locale, &state.config.locale) {
            self.has_editor_locales = Some(ctx.has_editor_locales);
            self.editor_locale = Some(ctx.editor_locale);
            self.editor_locales = Some(ctx.editor_locales);
        }

        self
    }
}

impl AuthBasePageContext {
    /// Construct the minimal base for an auth-flow page. No nav, no user.
    pub fn for_state(state: &AdminState, page: PageMeta) -> Self {
        let title = page.title.clone();
        let available_locales = state
            .translations
            .available_locales()
            .into_iter()
            .map(str::to_string)
            .collect();

        Self {
            crap: CrapMeta::for_auth_page(state),
            locale: state.config.locale.default_locale.clone(),
            available_locales,
            title,
            page,
        }
    }
}

/// Filter sidebar nav entries to only show collections/globals the current
/// user can read. The nav data is built unfiltered from the registry, then
/// trimmed against each collection's / global's `access.read` rule.
fn filter_nav_in_place(
    nav: &mut NavData,
    state: &AdminState,
    user_doc: Option<&crate::core::Document>,
) {
    nav.collections.retain(|c| {
        let access_ref = state
            .registry
            .collections
            .get(c.slug.as_str())
            .and_then(|d| d.access.read.as_deref());
        has_read_access(state, access_ref, user_doc)
    });

    nav.globals.retain(|g| {
        let access_ref = state
            .registry
            .globals
            .get(g.slug.as_str())
            .and_then(|d| d.access.read.as_deref());
        has_read_access(state, access_ref, user_doc)
    });
}

// Tests for the typed bases live alongside per-page-context tests once the
// handlers migrate (each page test exercises its base via `for_handler` and
// the real `AdminState`). Hand-constructing a `CrapMeta` here would
// duplicate that fixture.
