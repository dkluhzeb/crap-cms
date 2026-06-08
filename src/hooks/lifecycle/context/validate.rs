//! Context passed to `validate` field hooks.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::core::Document;
use crate::typegen::lua::LuaAnnotation;

/// Context passed to `validate` field hooks.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.ValidateContext")]
pub struct ValidateContext<'a> {
    /// Collection slug.
    pub collection: &'a str,
    /// Name of the field being validated.
    pub field_name: &'a str,
    /// The operation being validated: `"create"` or `"update"`. Lets a custom
    /// validator branch (e.g. "immutable after create", or stricter on update).
    pub operation: &'a str,
    /// The document id on `update` (the row being edited); `nil` on `create`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "string", optional)]
    pub id: Option<&'a str>,
    /// The data in the *nearest scope*: the full document for a top-level field,
    /// or the current row for a validator on a field inside an array/blocks row.
    #[lua(ty = "table<string, any>")]
    pub data: &'a HashMap<String, JsonValue>,
    /// The full document being written. Equals `data` for top-level fields; for
    /// a sub-field validator inside an array/blocks row it is the *parent*
    /// document, so the validator can cross-reference fields outside its row.
    #[lua(ty = "table<string, any>")]
    pub document: &'a HashMap<String, JsonValue>,
    /// Authenticated user document (nil if unauthenticated or no auth
    /// collection).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table", optional)]
    pub user: Option<&'a Document>,
    /// Admin UI locale code (e.g., `"en"`, `"de"`). Nil if not set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_locale: Option<&'a str>,
    /// The content locale this write targets (e.g. `"en"`, `"de"`) — the
    /// requested locale, or the default when none was given. Nil when
    /// localization is disabled. Lets a custom validator enforce per-locale
    /// rules (e.g. only require a value in the default locale).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<&'a str>,
}
