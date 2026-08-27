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
        handlers::shared::{has_access_with_conn, is_admin_visible_with_conn},
    },
    core::auth::{AuthUser, Claims},
    typegen::LuaAnnotation,
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

impl LuaAnnotation for BasePageContext {
    /// The aggregate template context — references every other Lua class
    /// emitted by [`render_template_data_types`]. Per-page extra fields
    /// (e.g. `collection_cards`, `versions`) aren't enumerated here; the
    /// generated file points readers at the per-page reference docs.
    ///
    /// Hand-written: the annotation injects fields (`collection`, `global`,
    /// `document`) that don't live on the struct, plus a trailing comment
    /// block. The derive can't reproduce that shape, so this impl stays
    /// manual.
    ///
    /// [`render_template_data_types`]: crate::typegen::lua::render
    const CLASS_NAME: &'static str = "crap.template_ctx";

    fn render_lua_annotation(out: &mut String) {
        out.push_str("---@class crap.template_ctx\n");
        out.push_str("---@field crap crap.template.crap_meta\n");
        out.push_str("---@field _locale string\n");
        out.push_str("---@field available_locales string[]\n");
        out.push_str("---@field title string\n");
        out.push_str("---@field page crap.template.page\n");
        out.push_str("---@field nav crap.template.nav\n");
        out.push_str("---@field breadcrumbs? crap.template.breadcrumb[]\n");
        out.push_str("---@field user? crap.template.user\n");
        out.push_str("---@field collection? crap.template.collection\n");
        out.push_str("---@field global? crap.template.global\n");
        out.push_str("---@field document? crap.template.document\n");
        out.push_str("---@field has_editor_locales? boolean\n");
        out.push_str("---@field editor_locale? string\n");
        out.push_str("---@field editor_locales? crap.template.editor_locale_option[]\n");
        out.push_str(
            "-- For page-specific fields beyond the bases (e.g. `collection_cards`, `versions`),\n",
        );
        out.push_str(
            "-- see docs/src/admin-ui/template-context.md or the generated reference.\n\n",
        );
    }
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
        auth_user: Option<&Extension<AuthUser>>,
        page: PageMeta,
    ) -> Self {
        let user = claims.map(UserContext::from_claims);
        let locale = auth_user.map_or_else(
            || state.config.locale.default_locale.clone(),
            |Extension(au)| au.ui_locale.clone(),
        );
        let user_doc = auth_user.map(|Extension(au)| &au.user_doc);

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
        self.breadcrumbs.clone_from(&breadcrumbs);
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

/// Filter sidebar nav entries to only show collections/globals/custom-pages
/// the current user can read. Nav data is built unfiltered from the
/// registries, then trimmed against each entry's access rule.
fn filter_nav_in_place(
    nav: &mut NavData,
    state: &AdminState,
    user_doc: Option<&crate::core::Document>,
) {
    // `access.read` gates whether the entry is readable at all; `access.admin`
    // (permissive when unset) further restricts admin-UI visibility, evaluated
    // under op "admin" to match the route middleware — the real gate. The
    // dashboard cards apply the same `is_admin_visible` filter so nav and
    // dashboard stay in lockstep.
    //
    // All entries share ONE transaction — this runs on every authenticated page,
    // once per collection/global/custom-page, so a pooled connection per check
    // would fan out into many short-lived transactions under load. If a
    // connection can't be acquired, hide everything (fail-safe, matching the
    // per-entry behavior the pooled path had).
    let Ok(mut conn) = state.infra.pool.get() else {
        nav.collections.clear();
        nav.globals.clear();
        nav.custom_pages.clear();
        return;
    };
    let Ok(tx) = conn.transaction() else {
        nav.collections.clear();
        nav.globals.clear();
        nav.custom_pages.clear();
        return;
    };

    nav.collections.retain(|c| {
        let def = state.infra.registry.collections.get(c.slug.as_str());

        is_admin_visible_with_conn(
            state,
            def.and_then(|d| d.access.read.as_ref()),
            def.and_then(|d| d.access.admin.as_ref()),
            user_doc,
            &tx,
            &c.slug,
        )
    });

    nav.globals.retain(|g| {
        let def = state.infra.registry.globals.get(g.slug.as_str());

        is_admin_visible_with_conn(
            state,
            def.and_then(|d| d.access.read.as_ref()),
            def.and_then(|d| d.access.admin.as_ref()),
            user_doc,
            &tx,
            &g.slug,
        )
    });

    nav.custom_pages
        .retain(|p| has_access_with_conn(state, p.access.as_ref(), user_doc, &tx, "read", ""));

    let _ = tx.commit();
}

// Tests for the typed bases live alongside per-page-context tests once the
// handlers migrate (each page test exercises its base via `for_handler` and
// the real `AdminState`). Hand-constructing a `CrapMeta` here would
// duplicate that fixture.
