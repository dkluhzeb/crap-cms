//! Context passed to admin display-condition functions
//! (`field.admin.condition`).

use serde::Serialize;

use crate::{core::Document, typegen::lua::LuaAnnotation};

/// Context passed as the **second** argument to a display-condition function:
/// `function(form_data, ctx)`. Lets a condition gate on who is editing, which
/// operation, and the locale — beyond the form values in the first argument.
/// Existing `function(form_data)` conditions keep working (Lua silently drops
/// the extra argument).
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.ConditionContext")]
pub struct ConditionContext<'a> {
    /// Collection (or global) slug the form belongs to.
    pub collection: &'a str,
    /// `"create"` or `"update"` — whether the form is creating a new document
    /// or editing an existing one.
    pub operation: &'a str,
    /// Authenticated admin user (nil if not resolvable). Typed as `crap.AuthUser`
    /// so a condition can read `ctx.user.role` etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "crap.AuthUser", optional)]
    pub user: Option<&'a Document>,
    /// Admin UI locale code (e.g. `"en"`, `"de"`). Nil if not set.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub ui_locale: Option<&'a str>,
    /// The content locale the form is editing (e.g. `"en"`, `"de"`). Nil when
    /// localization is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub locale: Option<&'a str>,
}
