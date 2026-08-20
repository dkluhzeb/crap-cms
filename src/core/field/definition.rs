//! Complete definition of a single field within a collection.

use crate::core::{
    BlockDefinition, FieldAdmin, FieldTab, FieldType, HookRef, RelationshipConfig, SelectOption,
    field::JoinConfig,
};
use crate::typegen::lua::{LuaAlias, LuaAnnotation, LuaFieldTypeViews, LuaTypeAlias};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `date` field appearance + storage format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, LuaAlias)]
#[serde(rename_all = "camelCase")]
#[lua(alias = "crap.PickerAppearance", rename_all = "camelCase")]
pub enum PickerAppearance {
    /// Date picker, stored as `YYYY-MM-DDT12:00:00.000Z`.
    DayOnly,
    /// Datetime-local picker, stored as full ISO 8601 UTC.
    DayAndTime,
    /// Time picker, stored as `HH:MM`.
    TimeOnly,
    /// Month picker, stored as `YYYY-MM`.
    MonthOnly,
}

impl PickerAppearance {
    /// Return the canonical camelCase Lua-side string for this variant.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DayOnly => "dayOnly",
            Self::DayAndTime => "dayAndTime",
            Self::TimeOnly => "timeOnly",
            Self::MonthOnly => "monthOnly",
        }
    }
}

/// Unrecognized `picker_appearance` string. Callers (the parser) typically
/// log a warning and treat the field as if no value was set, falling back
/// to the default (`DayOnly`).
#[derive(Debug)]
pub struct UnknownPickerAppearance(pub String);

impl std::fmt::Display for UnknownPickerAppearance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown picker_appearance '{}' (valid: dayOnly, dayAndTime, timeOnly, monthOnly)",
            self.0
        )
    }
}

impl std::error::Error for UnknownPickerAppearance {}

impl std::str::FromStr for PickerAppearance {
    type Err = UnknownPickerAppearance;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dayOnly" => Ok(Self::DayOnly),
            "dayAndTime" => Ok(Self::DayAndTime),
            "timeOnly" => Ok(Self::TimeOnly),
            "monthOnly" => Ok(Self::MonthOnly),
            _ => Err(UnknownPickerAppearance(s.to_string())),
        }
    }
}

/// Custom validation function type.
/// Return `nil` (or `true`) if valid; return a string error message if invalid.
/// Used by `crap.FieldDefinition.validate` (a Lua function ref like
/// `"validators.foo"` that resolves to a function of this shape).
#[derive(LuaTypeAlias)]
#[lua(
    alias = "crap.ValidateFunction",
    target = "fun(value: any, context: crap.ValidateContext): string?"
)]
pub struct ValidateFunction;

/// Field hook function type.
/// Receives the field value and a context table; returns the (possibly
/// modified) value. Used by every `crap.FieldHooks` slot — the Lua
/// function ref stored there must resolve to a function of this shape.
#[derive(LuaTypeAlias)]
#[lua(
    alias = "crap.FieldHookFn",
    target = "fun(value: any, context: crap.FieldHookContext): any"
)]
pub struct FieldHookFn;

/// Lua function references for field-level access control (read/create/update).
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[lua(class = "crap.FieldAccess")]
pub struct FieldAccess {
    /// Hook ref for field read access control.
    #[serde(default)]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub read: Option<HookRef>,
    /// Hook ref for field create access control.
    #[serde(default)]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub create: Option<HookRef>,
    /// Hook ref for field update access control.
    #[serde(default)]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub update: Option<HookRef>,
}

/// Lua function references for field-level lifecycle hooks.
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[lua(class = "crap.FieldHooks")]
pub struct FieldHooks {
    /// Hook refs to run before field validation (value normalizers).
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub before_validate: Vec<HookRef>,
    /// Hook refs to run after validation, before write.
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub before_change: Vec<HookRef>,
    /// Hook refs to run after create/update write.
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub after_change: Vec<HookRef>,
    /// Hook refs to run after read, before response.
    #[serde(default)]
    #[lua(ty = "(string | crap.HookRef)[]", optional)]
    pub after_read: Vec<HookRef>,
}

impl FieldHooks {
    /// `true` when no lifecycle hook of any kind is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.before_validate.is_empty()
            && self.before_change.is_empty()
            && self.after_change.is_empty()
            && self.after_read.is_empty()
    }
}

/// Which locales a `required` *localized* field must be filled in for a
/// document to be considered complete. `All` expands to every configured
/// locale; `List` names specific locales. Resolved against `LocaleConfig` at
/// validation time. When unset on a field, the collection-level default
/// applies; when that's also unset, only the default locale is required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequiredLocales {
    /// Every configured locale.
    All,
    /// A specific list of locale codes.
    List(Vec<String>),
}

impl Serialize for RequiredLocales {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            RequiredLocales::All => s.serialize_str("all"),
            RequiredLocales::List(v) => v.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for RequiredLocales {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            S(String),
            L(Vec<String>),
        }
        match Raw::deserialize(d)? {
            Raw::S(s) if s == "all" => Ok(RequiredLocales::All),
            Raw::S(s) => Err(serde::de::Error::custom(format!(
                "invalid required_locales '{s}': expected \"all\" or a list of locale codes"
            ))),
            Raw::L(v) => Ok(RequiredLocales::List(v)),
        }
    }
}

/// Complete definition of a single field within a collection.
/// Use the per-type factory classes (`crap.fields.text(...)`,
/// `crap.fields.select(...)`, etc.) for precise per-type
/// autocompletion; this catch-all class lists every option the system
/// understands.
#[derive(Debug, Clone, Default, Serialize, Deserialize, LuaFieldTypeViews, LuaAnnotation)]
#[lua(base = "crap.BaseField", discriminator = FieldType, class = "crap.FieldDefinition")]
pub struct FieldDefinition {
    /// Column name (required).
    pub name: String,
    /// Field type (required).
    #[serde(rename = "type")]
    #[lua(skip)]
    pub field_type: FieldType,
    /// Validation: must have a value (default: false).
    #[serde(default)]
    #[lua(optional)]
    pub required: bool,
    /// Conditional requirement: a Lua predicate ref (`"module.fn"`). When set,
    /// the field is required whenever the predicate returns truthy for the
    /// document being validated — in addition to a static `required = true`.
    /// The predicate receives the validate context (`ctx.data` = the full
    /// document), so it can require this field based on other fields' values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub required_when: Option<HookRef>,
    /// Unique constraint (default: false).
    #[serde(default)]
    #[lua(optional)]
    pub unique: bool,
    /// Create a B-tree index on this column (default: false). Skipped when unique=true.
    #[serde(default)]
    #[lua(optional)]
    pub index: bool,
    /// Lua function ref called as `crap.ValidateFunction`.
    #[serde(default)]
    #[lua(ty = "string | crap.HookRef", optional)]
    pub validate: Option<HookRef>,
    /// Default value on create.
    #[serde(default)]
    #[lua(ty = "any", optional)]
    pub default_value: Option<Value>,
    /// Option list (required).
    #[serde(default)]
    #[lua(applies_to = "select, radio", optional)]
    pub options: Vec<SelectOption>,
    /// Admin UI display options.
    #[serde(default)]
    #[lua(optional)]
    pub admin: FieldAdmin,
    /// Per-field lifecycle hooks.
    #[serde(default)]
    #[lua(optional)]
    pub hooks: FieldHooks,
    /// Field-level access control (read/create/update).
    #[serde(default)]
    #[lua(optional)]
    pub access: FieldAccess,
    /// MCP tool schema options.
    #[serde(default)]
    #[lua(optional)]
    pub mcp: McpFieldConfig,
    /// Target collection and cardinality.
    #[serde(default)]
    #[lua(applies_to = "relationship, upload", optional)]
    pub relationship: Option<RelationshipConfig>,
    /// Sub-field definitions (required). For row/collapsible: promoted to parent level (no prefix).
    #[serde(default)]
    #[lua(
        applies_to = "array, group, row, collapsible",
        ty = "crap.FieldDefinition[]",
        optional
    )]
    pub fields: Vec<FieldDefinition>,
    /// Block type definitions (required).
    #[serde(default)]
    #[lua(applies_to = "blocks", optional)]
    pub blocks: Vec<BlockDefinition>,
    /// Tab definitions (required). Each tab has a label and fields.
    #[serde(default)]
    #[lua(applies_to = "tabs", optional)]
    pub tabs: Vec<FieldTab>,
    /// Per-locale values (default: false).
    #[serde(default)]
    #[lua(optional)]
    pub localized: bool,
    /// Which locales a `required` localized field must be filled in (the
    /// completeness rule). `"all"` = every configured locale; a list names
    /// specific locales. Unset → the collection default, else the default
    /// locale only. Only meaningful when both `required` and `localized`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[lua(ty = "\"all\" | string[]", optional)]
    pub required_locales: Option<RequiredLocales>,
    /// Input type: "dayOnly" (default), "dayAndTime", "timeOnly", "monthOnly".
    #[serde(default)]
    #[lua(applies_to = "date", ty = "crap.PickerAppearance", optional)]
    pub picker_appearance: Option<PickerAppearance>,
    /// Minimum rows. Validated on create/update.
    #[serde(default)]
    #[lua(applies_to = "array, blocks", optional)]
    pub min_rows: Option<usize>,
    /// Maximum rows. Admin disables "Add" at max.
    #[serde(default)]
    #[lua(applies_to = "array, blocks", optional)]
    pub max_rows: Option<usize>,
    /// Minimum string length. Validated server-side + HTML minlength.
    #[serde(default)]
    #[lua(applies_to = "text, textarea", optional)]
    pub min_length: Option<usize>,
    /// Maximum string length. Validated server-side + HTML maxlength.
    #[serde(default)]
    #[lua(applies_to = "text, textarea", optional)]
    pub max_length: Option<usize>,
    /// Minimum value. Validated server-side + HTML min attr.
    #[serde(default)]
    #[lua(applies_to = "number", optional)]
    pub min: Option<f64>,
    /// Maximum value. Validated server-side + HTML max attr.
    #[serde(default)]
    #[lua(applies_to = "number", optional)]
    pub max: Option<f64>,
    /// Restrict a `number` field to whole values: fractional input is rejected
    /// at validation and the admin renders an integer stepper. Storage stays
    /// floating-point (exact for the realistic `±2^53` range).
    #[serde(default)]
    #[lua(applies_to = "number", optional)]
    pub integer: bool,
    /// Multi-value tag input. Stored as JSON array in TEXT column (text/number) or multi-select dropdown (select).
    #[serde(default)]
    #[lua(applies_to = "text, number, select", optional)]
    pub has_many: bool,
    /// Minimum date (ISO "YYYY-MM-DD").
    #[serde(default)]
    #[lua(applies_to = "date", optional)]
    pub min_date: Option<String>,
    /// Maximum date (ISO "YYYY-MM-DD").
    #[serde(default)]
    #[lua(applies_to = "date", optional)]
    pub max_date: Option<String>,
    /// Store an IANA timezone alongside the value. Requires `picker_appearance = "dayAndTime"` (ignored with a warning otherwise). Creates a companion `{field}_tz` column.
    #[serde(default)]
    #[lua(applies_to = "date", optional)]
    pub timezone: bool,
    /// IANA zone used as the admin form default (e.g. "`America/New_York`"). Only applies when `timezone = true`.
    #[serde(default)]
    #[lua(applies_to = "date", optional)]
    pub default_timezone: Option<String>,
    /// Target collection slug (required). Field on target collection that references this document (required).
    //
    // `flatten` inlines `JoinConfig`'s fields (`collection`, `on`)
    // directly onto `crap.JoinField` instead of emitting a single
    // `--- @field join? crap.JoinConfig` line — matches the Lua surface
    // where they're top-level on the join field config.
    #[serde(default)]
    #[lua(applies_to = "join", flatten)]
    pub join: Option<JoinConfig>,
    /// Strip from all read responses (gRPC/Lua/MCP/admin/REST) and skip in the admin form. For admin-form-only hiding (value still returned in API), use `admin.hidden` instead. Default: false.
    #[serde(default)]
    #[lua(optional)]
    pub hidden: bool,
}

impl FieldDefinition {
    /// Create a new `FieldDefinitionBuilder` with the given name and type.
    pub fn builder(name: impl Into<String>, field_type: FieldType) -> FieldDefinitionBuilder {
        FieldDefinitionBuilder::new(name, field_type)
    }

    /// Whether this field has a column on the parent table.
    /// False for Array, Group, Row, Blocks, and has-many Relationship (they use join tables or prefixed/promoted columns).
    #[must_use]
    pub fn has_parent_column(&self) -> bool {
        match self.field_type {
            // Array uses a join table; Group prefixes sub-field columns; Row/Collapsible/Tabs
            // promote sub-fields to parent level (no prefix); Blocks uses a join table; Join is
            // a virtual field with no column.
            FieldType::Array
            | FieldType::Group
            | FieldType::Row
            | FieldType::Collapsible
            | FieldType::Tabs
            | FieldType::Blocks
            | FieldType::Join => false,
            FieldType::Relationship | FieldType::Upload => {
                match &self.relationship {
                    Some(rc) => !rc.has_many,
                    None => true, // default to has-one
                }
            }
            _ => true,
        }
    }

    /// Whether this field is a **scalar** has-many list (`Text` / `Number` /
    /// `Select` / `Radio` with `has_many = true`) — stored as a JSON array in its
    /// own main column rather than in a join table.
    ///
    /// A has-many *relationship* / *upload* is relational (`has_parent_column`
    /// is false), so scalar has-many is exactly `has_many && has_parent_column()`.
    /// Such a column must be `TEXT` regardless of the base type — a JSON array
    /// does not fit a numeric column, which silently works on `SQLite` (dynamic
    /// typing) but fails on Postgres. The stored/read value is a JSON array whose
    /// elements are canonicalized to the field's own type.
    #[must_use]
    pub fn is_has_many_scalar(&self) -> bool {
        self.has_many && self.has_parent_column()
    }

    /// The field's display label: the explicit `admin.label` when set and
    /// non-empty, else the field name title-cased. One source so the admin editor
    /// and the DB-read walkers (back-references) render the same label — including
    /// treating an explicitly-empty label as absent (falls back to the name)
    /// rather than rendering blank.
    #[must_use]
    pub fn resolved_label(&self) -> String {
        self.admin
            .label
            .as_ref()
            .map(|ls| ls.resolve_default().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| to_title_case(&self.name))
    }

    /// Whether this field's stored value is scoped per-locale, given whether it
    /// inherits localization from an enclosing group.
    ///
    /// Column-backed fields inherit localization from a parent group (their
    /// column becomes `field__locale`), so inheritance counts. Join-backed
    /// fields (array / blocks / has-many) are locale-scoped **only** by their
    /// own `localized` flag — `resolve_join_locale` ignores inheritance — so a
    /// non-localized join field inside a localized group keeps shared rows.
    #[must_use]
    pub fn is_locale_scoped(&self, inherited_localized: bool) -> bool {
        if self.has_parent_column() {
            inherited_localized || self.localized
        } else {
            self.localized
        }
    }
}

/// Convert a `snake_case` identifier to Title Case.
///
/// Examples: `"my_field"` → `"My Field"`, `"site_settings"` → `"Site Settings"`.
/// Used to auto-generate human-readable labels from field and collection names.
#[must_use]
pub fn to_title_case(s: &str) -> String {
    s.split('_')
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// MCP-specific configuration for a field.
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[serde(default)]
#[lua(class = "crap.McpFieldConfig")]
pub struct McpFieldConfig {
    /// Description shown in MCP tool JSON Schema for this field.
    pub description: Option<String>,
}

/// Builder for [`FieldDefinition`].
///
/// `name` and `field_type` are taken in `new()`. All other fields default via
/// [`FieldDefinition::default()`].
pub struct FieldDefinitionBuilder {
    inner: FieldDefinition,
}

impl FieldDefinitionBuilder {
    /// Create a new `FieldDefinitionBuilder` with the given name and type.
    pub fn new(name: impl Into<String>, field_type: FieldType) -> Self {
        Self {
            inner: FieldDefinition {
                name: name.into(),
                field_type,
                ..Default::default()
            },
        }
    }

    /// Set whether the field is required.
    #[must_use]
    pub fn required(mut self, v: bool) -> Self {
        self.inner.required = v;
        self
    }

    /// Set a Lua predicate ref that makes the field conditionally required.
    #[must_use]
    pub fn required_when(mut self, v: impl Into<HookRef>) -> Self {
        self.inner.required_when = Some(v.into());
        self
    }

    /// Set which locales a required localized field must be filled in.
    #[must_use]
    pub fn required_locales(mut self, v: RequiredLocales) -> Self {
        self.inner.required_locales = Some(v);
        self
    }

    /// Set whether the field must be unique.
    #[must_use]
    pub fn unique(mut self, v: bool) -> Self {
        self.inner.unique = v;
        self
    }

    /// Set whether to create a database index for this field.
    #[must_use]
    pub fn index(mut self, v: bool) -> Self {
        self.inner.index = v;
        self
    }

    /// Set the name of the Lua validation function.
    #[must_use]
    pub fn validate(mut self, v: impl Into<HookRef>) -> Self {
        self.inner.validate = Some(v.into());
        self
    }

    /// Set the default value for this field.
    #[must_use]
    pub fn default_value(mut self, v: Value) -> Self {
        self.inner.default_value = Some(v);
        self
    }

    /// Set the options for Select or Radio fields.
    #[must_use]
    pub fn options(mut self, v: Vec<SelectOption>) -> Self {
        self.inner.options = v;
        self
    }

    /// Set the admin UI configuration for this field.
    #[must_use]
    pub fn admin(mut self, v: FieldAdmin) -> Self {
        self.inner.admin = v;
        self
    }

    /// Set the lifecycle hooks for this field.
    #[must_use]
    pub fn hooks(mut self, v: FieldHooks) -> Self {
        self.inner.hooks = v;
        self
    }

    /// Set the access control rules for this field.
    #[must_use]
    pub fn access(mut self, v: FieldAccess) -> Self {
        self.inner.access = v;
        self
    }

    /// Set the MCP-specific configuration for this field.
    #[must_use]
    pub fn mcp(mut self, v: McpFieldConfig) -> Self {
        self.inner.mcp = v;
        self
    }

    /// Set the relationship configuration for this field.
    #[must_use]
    pub fn relationship(mut self, v: RelationshipConfig) -> Self {
        self.inner.relationship = Some(v);
        self
    }

    /// Set the sub-fields for Group or Array types.
    #[must_use]
    pub fn fields(mut self, v: Vec<FieldDefinition>) -> Self {
        self.inner.fields = v;
        self
    }

    /// Set the block definitions for Blocks types.
    #[must_use]
    pub fn blocks(mut self, v: Vec<BlockDefinition>) -> Self {
        self.inner.blocks = v;
        self
    }

    /// Set the tab definitions for Tabs types.
    #[must_use]
    pub fn tabs(mut self, v: Vec<FieldTab>) -> Self {
        self.inner.tabs = v;
        self
    }

    /// Set whether this field is localized.
    #[must_use]
    pub fn localized(mut self, v: bool) -> Self {
        self.inner.localized = v;
        self
    }

    /// Set the picker appearance for date fields.
    #[must_use]
    pub fn picker_appearance(mut self, v: PickerAppearance) -> Self {
        self.inner.picker_appearance = Some(v);
        self
    }

    /// Set the minimum number of rows for Array or Blocks.
    #[must_use]
    pub fn min_rows(mut self, v: usize) -> Self {
        self.inner.min_rows = Some(v);
        self
    }

    /// Set the maximum number of rows for Array or Blocks.
    #[must_use]
    pub fn max_rows(mut self, v: usize) -> Self {
        self.inner.max_rows = Some(v);
        self
    }

    /// Set the minimum string length for text fields.
    #[must_use]
    pub fn min_length(mut self, v: usize) -> Self {
        self.inner.min_length = Some(v);
        self
    }

    /// Set the maximum string length for text fields.
    #[must_use]
    pub fn max_length(mut self, v: usize) -> Self {
        self.inner.max_length = Some(v);
        self
    }

    /// Set the minimum numeric value.
    #[must_use]
    pub fn min(mut self, v: f64) -> Self {
        self.inner.min = Some(v);
        self
    }

    /// Set the maximum numeric value.
    #[must_use]
    pub fn max(mut self, v: f64) -> Self {
        self.inner.max = Some(v);
        self
    }

    /// Restrict a `number` field to whole values (reject fractional input).
    #[must_use]
    pub fn integer(mut self, v: bool) -> Self {
        self.inner.integer = v;
        self
    }

    /// Set whether this field allows multiple values.
    #[must_use]
    pub fn has_many(mut self, v: bool) -> Self {
        self.inner.has_many = v;
        self
    }

    /// Set the minimum date value.
    #[must_use]
    pub fn min_date(mut self, v: impl Into<String>) -> Self {
        self.inner.min_date = Some(v.into());
        self
    }

    /// Set the maximum date value.
    #[must_use]
    pub fn max_date(mut self, v: impl Into<String>) -> Self {
        self.inner.max_date = Some(v.into());
        self
    }

    /// Set whether to store an IANA timezone alongside the date value.
    #[must_use]
    pub fn timezone(mut self, v: bool) -> Self {
        self.inner.timezone = v;
        self
    }

    /// Set the default IANA timezone for the admin UI dropdown.
    #[must_use]
    pub fn default_timezone(mut self, v: impl Into<String>) -> Self {
        self.inner.default_timezone = Some(v.into());
        self
    }

    /// Set the join configuration for virtual reverse-relationship fields.
    #[must_use]
    pub fn join(mut self, v: JoinConfig) -> Self {
        self.inner.join = Some(v);
        self
    }

    /// Set whether this field is stripped from all read responses (API-hidden).
    /// See [`FieldDefinition::hidden`] for full semantics.
    #[must_use]
    pub fn hidden(mut self, v: bool) -> Self {
        self.inner.hidden = v;
        self
    }

    /// Build the final `FieldDefinition` instance.
    #[must_use]
    pub fn build(self) -> FieldDefinition {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::LocalizedString;
    use std::str::FromStr;

    // ── PickerAppearance FromStr + as_str ───────────────────────────

    #[test]
    fn picker_appearance_from_str_each_variant() {
        assert!(matches!(
            PickerAppearance::from_str("dayOnly"),
            Ok(PickerAppearance::DayOnly)
        ));
        assert!(matches!(
            PickerAppearance::from_str("dayAndTime"),
            Ok(PickerAppearance::DayAndTime)
        ));
        assert!(matches!(
            PickerAppearance::from_str("timeOnly"),
            Ok(PickerAppearance::TimeOnly)
        ));
        assert!(matches!(
            PickerAppearance::from_str("monthOnly"),
            Ok(PickerAppearance::MonthOnly)
        ));
    }

    #[test]
    fn picker_appearance_from_str_rejects_unknown_with_named_error() {
        let err = PickerAppearance::from_str("datetime").unwrap_err();
        assert!(err.to_string().contains("datetime"));
        assert!(err.to_string().contains("dayOnly"));
    }

    #[test]
    fn picker_appearance_as_str_is_canonical_camel_case() {
        assert_eq!(PickerAppearance::DayOnly.as_str(), "dayOnly");
        assert_eq!(PickerAppearance::DayAndTime.as_str(), "dayAndTime");
        assert_eq!(PickerAppearance::TimeOnly.as_str(), "timeOnly");
        assert_eq!(PickerAppearance::MonthOnly.as_str(), "monthOnly");
    }

    #[test]
    fn picker_appearance_serde_round_trip() {
        let pa = PickerAppearance::DayAndTime;
        let json = serde_json::to_string(&pa).unwrap();
        assert_eq!(json, "\"dayAndTime\"");
        let back: PickerAppearance = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, PickerAppearance::DayAndTime));
    }

    #[test]
    fn has_parent_column_scalar_types() {
        for ft in [
            FieldType::Text,
            FieldType::Number,
            FieldType::Textarea,
            FieldType::Select,
            FieldType::Checkbox,
            FieldType::Date,
            FieldType::Email,
            FieldType::Json,
            FieldType::Richtext,
            FieldType::Upload,
        ] {
            let f = FieldDefinition {
                field_type: ft.clone(),
                ..Default::default()
            };
            assert!(f.has_parent_column(), "{ft:?} should have parent column");
        }
    }

    #[test]
    fn has_parent_column_array_false() {
        let f = FieldDefinition {
            field_type: FieldType::Array,
            ..Default::default()
        };
        assert!(!f.has_parent_column());
    }

    #[test]
    fn has_parent_column_group_false() {
        let f = FieldDefinition {
            field_type: FieldType::Group,
            ..Default::default()
        };
        assert!(!f.has_parent_column());
    }

    #[test]
    fn has_parent_column_blocks_false() {
        let f = FieldDefinition {
            field_type: FieldType::Blocks,
            ..Default::default()
        };
        assert!(!f.has_parent_column());
    }

    #[test]
    fn has_parent_column_row_false() {
        let f = FieldDefinition {
            field_type: FieldType::Row,
            ..Default::default()
        };
        assert!(!f.has_parent_column(), "Row should not have parent column");
    }

    #[test]
    fn has_parent_column_collapsible_false() {
        let f = FieldDefinition {
            field_type: FieldType::Collapsible,
            ..Default::default()
        };
        assert!(
            !f.has_parent_column(),
            "Collapsible should not have parent column"
        );
    }

    #[test]
    fn has_parent_column_tabs_false() {
        let f = FieldDefinition {
            field_type: FieldType::Tabs,
            ..Default::default()
        };
        assert!(!f.has_parent_column(), "Tabs should not have parent column");
    }

    #[test]
    fn has_parent_column_relationship_has_one() {
        let f = FieldDefinition {
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig::new("posts", false)),
            ..Default::default()
        };
        assert!(
            f.has_parent_column(),
            "has-one relationship should have parent column"
        );
    }

    #[test]
    fn has_parent_column_relationship_has_many() {
        let f = FieldDefinition {
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig::new("tags", true)),
            ..Default::default()
        };
        assert!(
            !f.has_parent_column(),
            "has-many relationship should not have parent column"
        );
    }

    #[test]
    fn has_parent_column_relationship_no_config() {
        let f = FieldDefinition {
            field_type: FieldType::Relationship,
            relationship: None,
            ..Default::default()
        };
        assert!(
            f.has_parent_column(),
            "relationship with no config defaults to has-one"
        );
    }

    #[test]
    fn has_parent_column_upload_has_many_false() {
        let f = FieldDefinition {
            field_type: FieldType::Upload,
            relationship: Some(RelationshipConfig::new("media", true)),
            ..Default::default()
        };
        assert!(
            !f.has_parent_column(),
            "has-many upload should not have parent column"
        );
    }

    #[test]
    fn has_parent_column_upload_has_one_true() {
        let f = FieldDefinition {
            field_type: FieldType::Upload,
            relationship: Some(RelationshipConfig::new("media", false)),
            ..Default::default()
        };
        assert!(
            f.has_parent_column(),
            "has-one upload should have parent column"
        );
    }

    #[test]
    fn field_definition_default() {
        let f = FieldDefinition::default();
        assert_eq!(f.name, "");
        assert_eq!(f.field_type, FieldType::Text);
        assert!(!f.required);
        assert!(!f.unique);
        assert!(!f.index);
        assert!(!f.localized);
        assert!(f.validate.is_none());
        assert!(f.default_value.is_none());
        assert!(f.options.is_empty());
        assert!(f.relationship.is_none());
        assert!(f.fields.is_empty());
        assert!(f.blocks.is_empty());
        assert!(f.tabs.is_empty());
        assert!(f.min_rows.is_none());
        assert!(f.max_rows.is_none());
        assert!(f.min_length.is_none());
        assert!(f.max_length.is_none());
        assert!(f.min.is_none());
        assert!(f.max.is_none());
    }

    #[test]
    fn to_title_case_single_word() {
        assert_eq!(to_title_case("posts"), "Posts");
    }

    #[test]
    fn to_title_case_multi_word() {
        assert_eq!(to_title_case("site_settings"), "Site Settings");
    }

    #[test]
    fn to_title_case_three_words() {
        assert_eq!(to_title_case("my_cool_thing"), "My Cool Thing");
    }

    #[test]
    fn to_title_case_empty() {
        assert_eq!(to_title_case(""), "");
    }

    #[test]
    fn to_title_case_double_underscore() {
        assert_eq!(to_title_case("seo__title"), "Seo Title");
    }

    #[test]
    fn builds_field_definition_with_defaults() {
        let fd = FieldDefinitionBuilder::new("title", FieldType::Text).build();
        assert_eq!(fd.name, "title");
        assert_eq!(fd.field_type, FieldType::Text);
        assert!(!fd.required);
        assert!(!fd.unique);
        assert!(fd.options.is_empty());
        assert!(fd.relationship.is_none());
        assert!(fd.fields.is_empty());
    }

    #[test]
    fn builds_field_definition_with_overrides() {
        let fd = FieldDefinitionBuilder::new("email", FieldType::Email)
            .required(true)
            .unique(true)
            .index(true)
            .max_length(255)
            .build();
        assert_eq!(fd.name, "email");
        assert_eq!(fd.field_type, FieldType::Email);
        assert!(fd.required);
        assert!(fd.unique);
        assert!(fd.index);
        assert_eq!(fd.max_length, Some(255));
    }

    #[test]
    fn builds_field_definition_with_relationship() {
        let fd = FieldDefinitionBuilder::new("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("users", false))
            .build();
        assert!(fd.relationship.is_some());
        assert_eq!(fd.relationship.unwrap().collection, "users");
    }

    #[test]
    fn builds_field_definition_with_has_many() {
        let fd = FieldDefinitionBuilder::new("tags", FieldType::Select)
            .has_many(true)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("A".into()), "a"),
                SelectOption::new(LocalizedString::Plain("B".into()), "b"),
            ])
            .build();
        assert!(fd.has_many);
        assert_eq!(fd.options.len(), 2);
    }

    #[test]
    fn is_has_many_scalar_true_for_scalar_lists() {
        for ft in [
            FieldType::Text,
            FieldType::Number,
            FieldType::Select,
            FieldType::Radio,
        ] {
            let fd = FieldDefinitionBuilder::new("f", ft.clone())
                .has_many(true)
                .build();
            assert!(fd.is_has_many_scalar(), "{ft:?}");
        }
    }

    #[test]
    fn is_has_many_scalar_false_without_has_many() {
        let fd = FieldDefinitionBuilder::new("f", FieldType::Number).build();
        assert!(!fd.is_has_many_scalar());
    }

    /// A has-many *relationship* is relational (join table), not a scalar list —
    /// `has_parent_column()` is false, so it must be excluded.
    #[test]
    fn is_has_many_scalar_false_for_relationship() {
        let fd = FieldDefinitionBuilder::new("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("users", true))
            .build();
        assert!(fd.has_many || fd.relationship.as_ref().unwrap().has_many);
        assert!(!fd.is_has_many_scalar());
    }
}
