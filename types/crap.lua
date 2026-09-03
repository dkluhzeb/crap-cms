---@meta
--- Crap CMS Lua API type definitions for lua-language-server (LuaLS).
---
--- This file is NOT executed at runtime. It provides type annotations
--- for IDE autocompletion, hover docs, and type checking.
---
--- Add this to your LuaLS workspace library or .luarc.json:
---   { "Lua.workspace.library": ["./types"] }

--- @class crap
--- The global `crap` table — entry point for all CMS operations.
--- Available in init.lua, collection definitions, and hook functions.
crap = {}


--- Supported field types for collection and global definitions.
--- @alias crap.FieldType
--- | "text"         # Single-line string
--- | "number"       # Integer or float
--- | "textarea"     # Multi-line text
--- | "richtext"     # Rich text (stored as HTML by default, or JSON with admin.format = "json")
--- | "select"       # Single or multi select from options (`has_many` for multi)
--- | "radio"        # Radio button group (same as select, renders as radio buttons)
--- | "checkbox"     # Boolean (true/false)
--- | "date"         # ISO 8601 date/datetime
--- | "email"        # Validated email address
--- | "json"         # Arbitrary JSON blob
--- | "upload"       # File upload (references media collection; `has_many` for multi-file)
--- | "relationship" # Reference to another collection
--- | "array"        # Repeatable sub-fields
--- | "group"        # Visual grouping (no extra table)
--- | "blocks"       # Flexible content blocks
--- | "row"          # Layout-only horizontal grouping (no prefix)
--- | "collapsible"  # Layout-only collapsible section (no prefix)
--- | "tabs"         # Layout-only tabbed container (no prefix)
--- | "code"         # Code editor (`CodeMirror`, `admin.language` for mode)
--- | "join"         # Virtual reverse relationship (read-only, no column)

--- A string that can be plain or per-locale.
--- Plain: `"Title"` — used as-is.
--- Localized: `{ en = "Title", de = "Titel" }` — resolved based on admin locale.
--- @alias crap.LocalizedString string | table<string, string>

--- Field width on the admin form. The three named variants line up
--- with the `form__field--*` CSS classes; `Custom` accepts any CSS
--- width value (e.g. `"50%"`, `"200px"`) for cases the presets don't
--- cover — passed through unchanged to the rendered field's
--- `style.width`.
--- @alias crap.FieldWidth "full" | "half" | "third" | string

--- Configuration for relationship fields (target collection, cardinality, depth cap).
--- @class crap.RelationshipConfig
--- @field collection string|string[] Target collection slug, or an array of slugs for polymorphic relationships (required). Example: `"posts"` or `{ "posts", "pages" }`.
--- @field has_many? boolean Many-to-many relationship via junction table (default: false).
--- @field max_depth? integer Per-field max population depth. Limits depth regardless of request-level depth.

--- A label/value pair for select field options.
--- @class crap.SelectOption
--- @field label crap.LocalizedString Display text in the admin UI.
--- @field value string Stored value.

--- Admin UI display hints for a field (placeholder, description, visibility, width).
--- @class crap.FieldAdmin
--- @field label? crap.LocalizedString UI label (defaults to field name).
--- @field placeholder? crap.LocalizedString Input placeholder text.
--- @field description? crap.LocalizedString Help text shown below the input.
--- @field hidden? boolean Hide from the admin edit form only (default: false). The field's value is still returned in API responses (gRPC, Lua, MCP, REST). For full API stripping, use top-level `hidden` on `crap.FieldDefinition` instead.
--- @field readonly? boolean Non-editable in admin (default: false).
--- @field width? crap.FieldWidth Field width: `"full"`, `"half"`, `"third"`, or an arbitrary CSS value (e.g. `"50%"`, `"200px"`).
--- @field collapsed? boolean Start collapsed in admin UI — groups, collapsibles, array/block rows (default: true). Set `false` to start expanded.
--- @field label_field? string Sub-field name to use as row label in admin (arrays/blocks). The value of this sub-field is shown as the row title. For blocks, per-block `label_field` on `BlockDefinition` takes priority.
--- @field row_label? string Lua function ref for computed row labels (arrays/blocks). Receives the row data table, returns a display string or nil. Takes priority over `label_field`. Signature: `fun(row: table): string?`.
--- @field labels? crap.FieldAdminLabels Custom singular/plural labels for row items (e.g., `{ singular = "Slide", plural = "Slides" }` → "Add Slide" button).
--- @field position? string "main" or "sidebar".
--- @field condition? string | crap.HookRef Lua function ref (string) for conditional show/hide — e.g. `"hooks.conditions.show_external_url"`. Inline condition tables are not accepted here; this field is the name of the function, not the condition itself. The referenced function receives form data and returns either: - a boolean (server-evaluated on each change via HTMX), or - a condition table (serialized to JSON, client-evaluated instantly). See `docs/src/admin-ui/guides/display-conditions.md`.
--- @field step? string Step value for number inputs (default: "any"). Use "1" for integers, "0.01" for cents, etc.
--- @field rows? integer Number of rows for textarea fields (default: 8).
--- @field language? string Default language mode for code fields (default: "json"). Options: "json", "javascript", "html", "css", "python", "plain". When `languages` is non-empty, this is the initial value; the editor can switch to any other language in the allow-list at edit time.
--- @field languages? string[] Allow-list of languages the editor can pick from at edit time for code fields. When set, the form renders a `<select>` next to the editor and persists the choice in a `<name>_lang` companion column. When empty/absent, the language is fixed to `language`.
--- @field features? string[] Enabled toolbar features for richtext fields. When absent, all features are enabled. Options: "bold", "italic", "code", "link", "heading", "blockquote", "orderedList", "bulletList", "codeBlock", "horizontalRule".
--- @field picker? string Picker UI style. For blocks fields: "select" (default) uses a dropdown, "card" uses a visual card grid. For upload fields: "drawer" (default) adds a browse button with thumbnail grid, "none" disables it. For relationship fields: "drawer" adds a browse button with searchable list.
--- @field format? string Storage format for richtext fields: "html" (default) or "json" (`ProseMirror` JSON). FTS extracts plain text from JSON automatically.
--- @field nodes? string[] Custom `ProseMirror` node types for richtext fields. Names must match nodes registered via `crap.richtext.register_node()`.
--- @field resizable? boolean Allow vertical resize on textarea/richtext fields (default: true).
--- @field template? string Custom template name to render this field instead of the default `fields/<field_type>` lookup. The path is relative to `templates/` (no `.hbs` extension), e.g. `"fields/rating"` or `"fields/custom/star-picker"`. Provides a per-instance opt-out from the type-based template routing in `RenderFieldHelper` so an individual field can declare its own admin UI without requiring a global override of `templates/fields/<type>.hbs`. The custom template receives the same field render context as the built-in template at that path. Restricted to safe characters (`a-zA-Z0-9/_-`); `..` and absolute paths are rejected.
--- @field extra? table<string, any> Freeform per-field configuration map, available to the field's admin template at `{{admin.extra.<key>}}`. Pairs naturally with [`Self::template`] — a custom template reads its config from `extra` so the same template + JS component can be reused across fields with different settings (icon, color, swatches, step labels, …) without per-field forking. Values are JSON-serializable (string / number / bool / array / nested object). For dynamic values (computed at render time), register a [`crap.template_data`](crate::admin::templates) function and pull from `{{data "name"}}` in the template instead — `extra` is parsed once at field-definition time and is **static** per field instance. Empty by default. Empty maps don't serialize (`skip_serializing_if`), keeping schema dumps and roundtrips clean.

--- Custom validation function type.
--- Return `nil` (or `true`) if valid; return a string error message if invalid.
--- Used by `crap.FieldDefinition.validate` (a Lua function ref like
--- `"validators.foo"` that resolves to a function of this shape).
--- @alias crap.ValidateFunction fun(value: any, context: crap.ValidateContext): string?

--- Context passed to `validate` field hooks.
--- @class crap.ValidateContext
--- @field collection string Collection slug.
--- @field field_name string Name of the field being validated.
--- @field operation string The operation being validated: `"create"` or `"update"`. Lets a custom validator branch (e.g. "immutable after create", or stricter on update).
--- @field id? string The document id on `update` (the row being edited); `nil` on `create`.
--- @field data table<string, any> The data in the *nearest scope*: the full document for a top-level field, or the current row for a validator on a field inside an array/blocks row.
--- @field document table<string, any> The full document being written. Equals `data` for top-level fields; for a sub-field validator inside an array/blocks row it is the *parent* document, so the validator can cross-reference fields outside its row.
--- @field user? table Authenticated user document (nil if unauthenticated or no auth collection).
--- @field ui_locale? string Admin UI locale code (e.g., `"en"`, `"de"`). Nil if not set.
--- @field locale? string The content locale this write targets (e.g. `"en"`, `"de"`) — the requested locale, or the default when none was given. Nil when localization is disabled. Lets a custom validator enforce per-locale rules (e.g. only require a value in the default locale).
--- @field options? table Per-config options from the `validate` / `required_when` hook ref's `{ ref, options }` table; `nil` when configured as a bare ref string.

--- Lua function references for field-level lifecycle hooks.
--- @class crap.FieldHooks
--- @field before_validate? (string | crap.HookRef)[] Hook refs to run before field validation (value normalizers).
--- @field before_change? (string | crap.HookRef)[] Hook refs to run after validation, before write.
--- @field after_change? (string | crap.HookRef)[] Hook refs to run after create/update write.
--- @field after_read? (string | crap.HookRef)[] Hook refs to run after read, before response.

--- Field hook function type.
--- Receives the field value and a context table; returns the (possibly
--- modified) value. Used by every `crap.FieldHooks` slot — the Lua
--- function ref stored there must resolve to a function of this shape.
--- @alias crap.FieldHookFn fun(value: any, context: crap.FieldHookContext): any

--- Context passed to per-field hook functions.
--- @class crap.FieldHookContext
--- @field field_name string Name of the field being processed.
--- @field collection string Collection slug.
--- @field operation string The operation: `"create"`, `"update"`, `"find"`, `"find_by_id"`, …
--- @field id? string The id of the document being processed on `update` / read; `nil` on `create` (no row exists yet). Mirrors the `id` on the validator and access contexts.
--- @field locale? string Content locale for this operation (e.g. `"en"`, `"de"`). Nil when the operation is not locale-scoped. Distinct from `ui_locale` (the admin UI language) — this is the locale of the data being written or read.
--- @field data table<string, any> The **nearest scope** (read-only snapshot): the group object for a field inside a group, the current row for a field inside an array/blocks row, or the full document at the top level. Groups are nested objects, so a hook on a `seo.title` sub-field reads its sibling via `ctx.data.<sibling>`.
--- @field document table<string, any> The **full document** being written or read — a read-only snapshot taken before any field hook in this pass ran (so it does not reflect changes made by earlier field hooks in the same pass). Matches `data` at the top level; for a sub-field hook inside an array/blocks row it's the parent document, so the hook can cross-reference fields outside its row.
--- @field user? table Authenticated user document (nil if unauthenticated or no auth collection).
--- @field ui_locale? string Admin UI locale code (e.g., `"en"`, `"de"`). Nil if not set.
--- @field options? table Per-config options from this field hook's `{ ref, options }` table; `nil` when the hook was configured as a bare ref string.

--- Context passed as the **second** argument to a display-condition function:
--- `function(form_data, ctx)`. Lets a condition gate on who is editing, which
--- operation, and the locale — beyond the form values in the first argument.
--- Existing `function(form_data)` conditions keep working (Lua silently drops
--- the extra argument).
--- @class crap.ConditionContext
--- @field collection string Collection (or global) slug the form belongs to.
--- @field operation string `"create"` or `"update"` — whether the form is creating a new document or editing an existing one.
--- @field user? crap.AuthUser Authenticated admin user (nil if not resolvable). Typed as `crap.AuthUser` so a condition can read `ctx.user.role` etc.
--- @field ui_locale? string Admin UI locale code (e.g. `"en"`, `"de"`). Nil if not set.
--- @field locale? string The content locale the form is editing (e.g. `"en"`, `"de"`). Nil when localization is disabled.
--- @field options? table Per-config options from the condition's `{ ref, options }` table; `nil` when the condition was configured as a bare ref string.

--- A single tab within a Tabs layout field.
--- @class crap.FieldTab
--- @field label string Tab label displayed in the tab bar (required).
--- @field description? string Help text shown inside the tab panel.
--- @field fields crap.FieldDefinition[] Fields within this tab.

--- A block type definition for Blocks fields.
--- @class crap.BlockDefinition
--- @field type string Block type identifier (required).
--- @field fields crap.FieldDefinition[] Fields within this block type.
--- @field label? crap.LocalizedString Display label for the block type (defaults to type name).
--- @field label_field? string Sub-field name to use as row label for this block type. Overrides the field-level `admin.label_field` for blocks of this type.
--- @field group? string Group name for organizing blocks in the picker dropdown (rendered as `<optgroup>`).
--- @field image_url? string Image URL for displaying an icon/thumbnail in the block picker. When any block has an image, the picker renders as a visual card grid.

--- `date` field appearance + storage format.
--- @alias crap.PickerAppearance
--- | "dayOnly"    # Date picker, stored as `YYYY-MM-DDT12:00:00.000Z`.
--- | "dayAndTime" # Datetime-local picker, stored as full ISO 8601 UTC.
--- | "timeOnly"   # Time picker, stored as `HH:MM`.
--- | "monthOnly"  # Month picker, stored as `YYYY-MM`.

--- MCP-specific configuration for a field.
--- @class crap.McpFieldConfig
--- @field description? string Description shown in MCP tool JSON Schema for this field.

--- MCP-specific configuration for a collection or global.
--- @class crap.McpCollectionConfig
--- @field description? string Description used in MCP tool descriptions for this collection/global.
--- @field operations? table<string, string> Per-operation description overrides, keyed by operation name (e.g. `"create"`, `"delete"`, `"find"`; for globals `"read"` / `"update"` / `"validate"`). Overrides the auto-generated description for that tool.

--- Custom singular/plural labels for row items (arrays/blocks).
--- @class crap.FieldAdminLabels
--- @field singular? crap.LocalizedString Custom singular label for row items (e.g., "Slide" → "Add Slide" button).
--- @field plural? crap.LocalizedString Custom plural label for the field header.

--- Complete definition of a single field within a collection.
--- Use the per-type factory classes (`crap.fields.text(...)`,
--- `crap.fields.select(...)`, etc.) for precise per-type
--- autocompletion; this catch-all class lists every option the system
--- understands.
--- @class crap.BaseField
--- @field name string Column name (required).
--- @field required? boolean Validation: must have a value (default: false).
--- @field required_when? string | crap.HookRef Conditional requirement: a Lua predicate ref (`"module.fn"`). When set, the field is required whenever the predicate returns truthy for the document being validated — in addition to a static `required = true`. The predicate receives the validate context (`ctx.data` = the full document), so it can require this field based on other fields' values.
--- @field unique? boolean Unique constraint (default: false).
--- @field index? boolean Create a B-tree index on this column (default: false). Skipped when unique=true.
--- @field validate? string | crap.HookRef Lua function ref called as `crap.ValidateFunction`.
--- @field default_value? any Default value on create.
--- @field admin? crap.FieldAdmin Admin UI display options.
--- @field hooks? crap.FieldHooks Per-field lifecycle hooks.
--- @field access? crap.FieldAccess Field-level access control (read/create/update).
--- @field mcp? crap.McpFieldConfig MCP tool schema options.
--- @field localized? boolean Per-locale values (default: false).
--- @field required_locales? "all" | string[] Which locales a `required` localized field must be filled in (the completeness rule). `"all"` = every configured locale; a list names specific locales. Unset → the collection default, else the default locale only. Only meaningful when both `required` and `localized`.
--- @field hidden? boolean Strip from all read responses (gRPC/Lua/MCP/admin/REST) and skip in the admin form. For admin-form-only hiding (value still returned in API), use `admin.hidden` instead. Default: false.

--- @class crap.TextField : crap.BaseField
--- @field min_length? integer Minimum string length. Validated server-side + HTML minlength.
--- @field max_length? integer Maximum string length. Validated server-side + HTML maxlength.
--- @field has_many? boolean Multi-value tag input. Stored as JSON array in TEXT column (text/number) or multi-select dropdown (select).

--- @class crap.NumberField : crap.BaseField
--- @field min? number Minimum value. Validated server-side + HTML min attr.
--- @field max? number Maximum value. Validated server-side + HTML max attr.
--- @field integer? boolean Restrict a `number` field to whole values: fractional input is rejected at validation and the admin renders an integer stepper. Storage stays floating-point (exact for the realistic `±2^53` range).
--- @field has_many? boolean Multi-value tag input. Stored as JSON array in TEXT column (text/number) or multi-select dropdown (select).

--- @class crap.TextareaField : crap.BaseField
--- @field min_length? integer Minimum string length. Validated server-side + HTML minlength.
--- @field max_length? integer Maximum string length. Validated server-side + HTML maxlength.

--- @class crap.RichtextField : crap.BaseField

--- @class crap.SelectField : crap.BaseField
--- @field options? crap.SelectOption[] Option list (required).
--- @field has_many? boolean Multi-value tag input. Stored as JSON array in TEXT column (text/number) or multi-select dropdown (select).

--- @class crap.RadioField : crap.BaseField
--- @field options? crap.SelectOption[] Option list (required).

--- @class crap.CheckboxField : crap.BaseField

--- @class crap.DateField : crap.BaseField
--- @field picker_appearance? crap.PickerAppearance Input type: "dayOnly" (default), "dayAndTime", "timeOnly", "monthOnly".
--- @field min_date? string Minimum date (ISO "YYYY-MM-DD").
--- @field max_date? string Maximum date (ISO "YYYY-MM-DD").
--- @field timezone? boolean Store an IANA timezone alongside the value. Requires `picker_appearance = "dayAndTime"` (ignored with a warning otherwise). Creates a companion `{field}_tz` column.
--- @field default_timezone? string IANA zone used as the admin form default (e.g. "`America/New_York`"). Only applies when `timezone = true`.

--- @class crap.EmailField : crap.BaseField

--- @class crap.JsonField : crap.BaseField

--- @class crap.UploadField : crap.BaseField
--- @field relationship? crap.RelationshipConfig Target collection and cardinality.

--- @class crap.RelationshipField : crap.BaseField
--- @field relationship? crap.RelationshipConfig Target collection and cardinality.

--- @class crap.ArrayField : crap.BaseField
--- @field fields? crap.FieldDefinition[] Sub-field definitions (required). For row/collapsible: promoted to parent level (no prefix).
--- @field min_rows? integer Minimum rows. Validated on create/update.
--- @field max_rows? integer Maximum rows. Admin disables "Add" at max.

--- @class crap.GroupField : crap.BaseField
--- @field fields? crap.FieldDefinition[] Sub-field definitions (required). For row/collapsible: promoted to parent level (no prefix).

--- @class crap.BlocksField : crap.BaseField
--- @field blocks? crap.BlockDefinition[] Block type definitions (required).
--- @field min_rows? integer Minimum rows. Validated on create/update.
--- @field max_rows? integer Maximum rows. Admin disables "Add" at max.

--- @class crap.RowField : crap.BaseField
--- @field fields? crap.FieldDefinition[] Sub-field definitions (required). For row/collapsible: promoted to parent level (no prefix).

--- @class crap.CollapsibleField : crap.BaseField
--- @field fields? crap.FieldDefinition[] Sub-field definitions (required). For row/collapsible: promoted to parent level (no prefix).

--- @class crap.TabsField : crap.BaseField
--- @field tabs? crap.FieldTab[] Tab definitions (required). Each tab has a label and fields.

--- @class crap.CodeField : crap.BaseField

--- @class crap.JoinField : crap.BaseField
--- @field collection string Target collection slug (required).
--- @field on string Field on target collection that references this document (required).

--- Complete definition of a single field within a collection.
--- Use the per-type factory classes (`crap.fields.text(...)`,
--- `crap.fields.select(...)`, etc.) for precise per-type
--- autocompletion; this catch-all class lists every option the system
--- understands.
--- @class crap.FieldDefinition
--- @field name string Column name (required).
--- @field required? boolean Validation: must have a value (default: false).
--- @field required_when? string | crap.HookRef Conditional requirement: a Lua predicate ref (`"module.fn"`). When set, the field is required whenever the predicate returns truthy for the document being validated — in addition to a static `required = true`. The predicate receives the validate context (`ctx.data` = the full document), so it can require this field based on other fields' values.
--- @field unique? boolean Unique constraint (default: false).
--- @field index? boolean Create a B-tree index on this column (default: false). Skipped when unique=true.
--- @field validate? string | crap.HookRef Lua function ref called as `crap.ValidateFunction`.
--- @field default_value? any Default value on create.
--- @field options? crap.SelectOption[] Option list (required).
--- @field admin? crap.FieldAdmin Admin UI display options.
--- @field hooks? crap.FieldHooks Per-field lifecycle hooks.
--- @field access? crap.FieldAccess Field-level access control (read/create/update).
--- @field mcp? crap.McpFieldConfig MCP tool schema options.
--- @field relationship? crap.RelationshipConfig Target collection and cardinality.
--- @field fields? crap.FieldDefinition[] Sub-field definitions (required). For row/collapsible: promoted to parent level (no prefix).
--- @field blocks? crap.BlockDefinition[] Block type definitions (required).
--- @field tabs? crap.FieldTab[] Tab definitions (required). Each tab has a label and fields.
--- @field localized? boolean Per-locale values (default: false).
--- @field required_locales? "all" | string[] Which locales a `required` localized field must be filled in (the completeness rule). `"all"` = every configured locale; a list names specific locales. Unset → the collection default, else the default locale only. Only meaningful when both `required` and `localized`.
--- @field picker_appearance? crap.PickerAppearance Input type: "dayOnly" (default), "dayAndTime", "timeOnly", "monthOnly".
--- @field min_rows? integer Minimum rows. Validated on create/update.
--- @field max_rows? integer Maximum rows. Admin disables "Add" at max.
--- @field min_length? integer Minimum string length. Validated server-side + HTML minlength.
--- @field max_length? integer Maximum string length. Validated server-side + HTML maxlength.
--- @field min? number Minimum value. Validated server-side + HTML min attr.
--- @field max? number Maximum value. Validated server-side + HTML max attr.
--- @field integer? boolean Restrict a `number` field to whole values: fractional input is rejected at validation and the admin renders an integer stepper. Storage stays floating-point (exact for the realistic `±2^53` range).
--- @field has_many? boolean Multi-value tag input. Stored as JSON array in TEXT column (text/number) or multi-select dropdown (select).
--- @field min_date? string Minimum date (ISO "YYYY-MM-DD").
--- @field max_date? string Maximum date (ISO "YYYY-MM-DD").
--- @field timezone? boolean Store an IANA timezone alongside the value. Requires `picker_appearance = "dayAndTime"` (ignored with a warning otherwise). Creates a companion `{field}_tz` column.
--- @field default_timezone? string IANA zone used as the admin form default (e.g. "`America/New_York`"). Only applies when `timezone = true`.
--- @field join? crap.JoinConfig Target collection slug (required). Field on target collection that references this document (required).
--- @field hidden? boolean Strip from all read responses (gRPC/Lua/MCP/admin/REST) and skip in the admin form. For admin-form-only hiding (value still returned in API), use `admin.hidden` instead. Default: false.

--- Configuration for join (virtual reverse-relationship) fields.
--- @class crap.JoinConfig
--- @field collection string Target collection slug (required).
--- @field on string Field on target collection that references this document (required).

-- ── crap.fields ──────────────────────────────────────────────

--- Field factory functions. Each sets `type` automatically and returns a
--- `crap.FieldDefinition`-compatible table. Use these instead of raw tables
--- for precise per-type autocomplete.
---
--- Example:
--- ```lua
--- crap.collections.define("posts", {
---     fields = {
---         crap.fields.text({ name = "title", required = true }),
---         crap.fields.select({ name = "status", options = {
---             { label = "Draft", value = "draft" },
---             { label = "Published", value = "published" },
---         }}),
---         crap.fields.relationship({ name = "author", relationship = {
---             collection = "users",
---         }}),
---         crap.fields.blocks({ name = "content", blocks = { ... } }),
---     },
--- })
--- ```
--- @class crap.fields
crap.fields = {}

--- Create a text field (single-line string).
--- @param config crap.TextField
--- @return crap.FieldDefinition
function crap.fields.text(config) end

--- Create a number field (integer or float).
--- @param config crap.NumberField
--- @return crap.FieldDefinition
function crap.fields.number(config) end

--- Create a textarea field (multi-line text).
--- @param config crap.TextareaField
--- @return crap.FieldDefinition
function crap.fields.textarea(config) end

--- Create a richtext field (`ProseMirror` editor, stored as HTML by default or JSON).
--- @param config crap.RichtextField
--- @return crap.FieldDefinition
function crap.fields.richtext(config) end

--- Create a select field (dropdown, single or multi).
--- @param config crap.SelectField
--- @return crap.FieldDefinition
function crap.fields.select(config) end

--- Create a radio field (radio button group).
--- @param config crap.RadioField
--- @return crap.FieldDefinition
function crap.fields.radio(config) end

--- Create a checkbox field (boolean true/false).
--- @param config crap.CheckboxField
--- @return crap.FieldDefinition
function crap.fields.checkbox(config) end

--- Create a date field (ISO 8601 date/datetime).
--- @param config crap.DateField
--- @return crap.FieldDefinition
function crap.fields.date(config) end

--- Create an email field (validated email address).
--- @param config crap.EmailField
--- @return crap.FieldDefinition
function crap.fields.email(config) end

--- Create a JSON field (arbitrary JSON blob).
--- @param config crap.JsonField
--- @return crap.FieldDefinition
function crap.fields.json(config) end

--- Create a code field (`CodeMirror` editor). Set `admin.language` for syntax mode.
--- @param config crap.CodeField
--- @return crap.FieldDefinition
function crap.fields.code(config) end

--- Create a relationship field (reference to another collection).
--- @param config crap.RelationshipField
--- @return crap.FieldDefinition
function crap.fields.relationship(config) end

--- Create an upload field (file upload, references an upload collection).
--- @param config crap.UploadField
--- @return crap.FieldDefinition
function crap.fields.upload(config) end

--- Create an array field (repeatable sub-fields).
--- @param config crap.ArrayField
--- @return crap.FieldDefinition
function crap.fields.array(config) end

--- Create a group field (visual grouping with column prefix).
--- @param config crap.GroupField
--- @return crap.FieldDefinition
function crap.fields.group(config) end

--- Create a blocks field (flexible content blocks).
--- @param config crap.BlocksField
--- @return crap.FieldDefinition
function crap.fields.blocks(config) end

--- Create a row field (layout-only horizontal grouping, no column prefix).
--- @param config crap.RowField
--- @return crap.FieldDefinition
function crap.fields.row(config) end

--- Create a collapsible field (layout-only collapsible section, no column prefix).
--- @param config crap.CollapsibleField
--- @return crap.FieldDefinition
function crap.fields.collapsible(config) end

--- Create a tabs field (layout-only tabbed container, no column prefix).
--- @param config crap.TabsField
--- @return crap.FieldDefinition
function crap.fields.tabs(config) end

--- Create a join field (virtual reverse-relationship, read-only).
--- @param config crap.JoinField
--- @return crap.FieldDefinition
function crap.fields.join(config) end


--- Human-readable singular/plural labels for the admin UI.
--- @class crap.Labels
--- @field singular? crap.LocalizedString Singular display name (e.g., "Post" or `{ en = "Post", de = "Beitrag" }`).
--- @field plural? crap.LocalizedString Plural display name (e.g., "Posts" or `{ en = "Posts", de = "Beiträge" }`).

--- Admin UI display options (title field, default sort, visibility, searchable fields).
--- @class crap.AdminConfig
--- @field use_as_title? string Field name to use as row label in lists.
--- @field default_sort? string Default sort field (prefix with "-" for desc).
--- @field hidden? boolean Hide from admin sidebar (default: false).
--- @field list_searchable_fields? string[] Fields searchable in the list view.
--- @field list_columns? string[] Default columns shown in the list view, in order. Empty = the built-in default (`_status` if the collection has drafts, plus `created_at`). A per-user column selection overrides this. Entries may be field names or the meta columns `created_at` / `updated_at` / `_status`.

--- A reference to a Lua hook function, optionally carrying per-configuration
--- `options`.
---
--- One hook function can be reused across collections/fields with different
--- `options`; the resolved options table is exposed to the hook as
--- `ctx.options` (nil for a bare-string ref).
---
--- Both Lua config and serde accept two interchangeable shapes:
--- - a bare string `"hooks.posts.slugify"` (no options), and
--- - a table/map `{ ref = "hooks.posts.slugify", options = { ... } }`.
---
--- Serialization mirrors that: a ref without options serializes as a bare
--- string, so existing registry snapshots (which stored bare strings) round-trip
--- unchanged.
--- @class crap.HookRef
--- @field ref string Dotted Lua function reference, e.g. `"hooks.posts.slugify"`.
--- @field options? table Per-config options passed to the hook as `ctx.options` (nil when absent).

--- Lua function references for lifecycle hooks.
---
--- Each entry is a [`HookRef`]: a bare ref string or a `{ ref, options }` table
--- carrying per-config options exposed to the hook as `ctx.options`.
--- @class crap.Hooks
--- @field before_validate? (string | crap.HookRef)[] Hook refs to run before field validation.
--- @field before_change? (string | crap.HookRef)[] Hook refs to run before create/update write.
--- @field after_change? (string | crap.HookRef)[] Hook refs to run after create/update write.
--- @field before_read? (string | crap.HookRef)[] Hook refs to run before returning read results.
--- @field after_read? (string | crap.HookRef)[] Hook refs to run after read, before response.
--- @field before_delete? (string | crap.HookRef)[] Hook refs to run before delete.
--- @field after_delete? (string | crap.HookRef)[] Hook refs to run after delete.
--- @field before_broadcast? (string | crap.HookRef)[] Hook refs to run before broadcasting live update events. Can suppress or transform event data. No CRUD access.

--- Lua function references for access control (read/create/update/delete).
---
--- Each rule is a [`HookRef`]: a bare ref string or a `{ ref, options }` table
--- whose options reach the access function as `ctx.options`.
--- @class crap.Access
--- @field read? string | crap.HookRef Hook ref for read access control.
--- @field create? string | crap.HookRef Hook ref for create access control.
--- @field update? string | crap.HookRef Hook ref for update access control.
--- @field delete? string | crap.HookRef Hook ref for delete access control.
--- @field trash? string | crap.HookRef Hook ref for soft-delete (trash) access control. Falls back to `update` when unset, so most collections don't set this explicitly. Set to lock trashing behind a different policy than update — e.g. only editors can trash, but authors can still update their own drafts.
--- @field draft? string | crap.HookRef Hook ref for reading draft (unpublished) content — a read that opts into drafts (`draft = true` / `use_draft` / `include_drafts`). Falls back to `update` when unset, so previewing a draft requires edit-level access by default (drafts are not exposed to plain readers). Set to gate draft previews behind a different policy than editing.
--- @field versions? string | crap.HookRef Hook ref restricting access to version history — a *toggle*, not a per-snapshot filter. Like `trash`/`draft` it **falls back to `update`** when unset, so by default version history follows edit access: a published-only reader sees no history, while an editor does (no separate rule needed). Which *snapshots* are then visible still follows the per-snapshot composite (`read` for published snapshots, `draft` for draft snapshots). Set it to gate the version timeline behind a policy distinct from editing — e.g. let any reader inspect history, or restrict it to a subset of editors. The function returns `true`/`false` (`ctx`-based); returning a filter table is rejected (row-level scoping is `read`'s job).
--- @field unlock? string | crap.HookRef Hook ref gating the account lock-state operations (`LockAccount` / `UnlockAccount`) on an auth collection. Falls back to `update` when unset (like `trash`/`draft`), so by default locking another user requires the same edit-level access the admin UI already enforces. Set it to carve out a *narrower* privilege than full edit — e.g. a moderator who may block logins but not change a user's email or role. Shaped like `update`: a returned filter table scopes *which* users the caller may (un)lock (e.g. `{ org = ctx.user.org }`), enforced against the target user. Only meaningful on auth collections; rejected on globals.
--- @field admin? string | crap.HookRef Hook ref gating whether the collection is visible/usable in the **admin UI** (nav entry + its admin routes). A boolean rule (`true`/`false` based on `ctx.user`); a filter table is a configuration error (admin visibility is all-or-nothing — row scoping is `read`'s job). **Permissive default:** unset means visible, so it only ever *further* restricts admin-UI access beyond `read` — e.g. hide a collection's admin pages from non-staff even though an API client may read it. Independent of `default_deny`.
--- @field mcp? string | crap.HookRef Hook ref gating whether the collection is exposed to the **MCP** surface (tools + resources). A boolean rule; a filter table is a configuration error (the MCP surface authenticates with a shared key, not a per-user identity, so a `ctx.user`-keyed row filter is meaningless). **Permissive default:** unset means exposed (when MCP is enabled globally), so it only ever *removes* a collection from MCP — e.g. keep an internal collection out of the LLM surface. Independent of `default_deny`.

--- Context passed to `strategy`-type auth `authenticate` hooks.
--- @class crap.AuthStrategyContext
--- @field headers table<string, string> Request headers (lowercase keys).
--- @field collection string Auth collection slug.
--- @field email? string The submitted login identifier (email/username), when the strategy was reached via a password-style login. `nil` for header/token flows (OAuth).
--- @field password? string The submitted plaintext password, for strategies that verify credentials against an external system (LDAP, a remote API). `nil` for header/token flows. **Sensitive** — only your strategy hook receives it; never log it.
--- @field remote_addr? string The client's remote IP address, when known.
--- @field options? table Per-config options from the strategy's `authenticate` `{ ref, options }` table; `nil` when configured as a bare ref string.

--- Context passed to the `password_login` method's `mfa_when` gate hook.
--- The credentials are already verified when this runs; the hook only
--- decides whether THIS login must complete the second factor.
--- @class crap.MfaWhenContext
--- @field collection string Auth collection slug.
--- @field user table<string, any> The credential-verified user's field data (password hash hidden).
--- @field surface string The login surface: `"admin"` or `"grpc"`.
--- @field headers table<string, string> Request headers (lowercase keys).
--- @field options? table Per-config options from `{ ref, options }`; `nil` for a bare ref.

--- Context passed to the `password_login` method's `mfa_deliver` hook
--- (`mfa = "custom"`): send the code via your channel (SMS, push, chat, …).
--- The code is **sensitive** — never log it.
--- @class crap.MfaDeliverContext
--- @field collection string Auth collection slug.
--- @field user table<string, any> The credential-verified user's field data (password hash hidden).
--- @field code string The 6-digit code to deliver. Single-use; already stored server-side.
--- @field expires_in integer Seconds until the code expires.
--- @field options? table Per-config options from `{ ref, options }`; `nil` for a bare ref.

--- Context passed to the `live.filter` function — the per-collection gate that
--- decides whether a committed mutation is broadcast to live subscribers.
---
--- Runs **after** the write commits, with no CRUD/transaction access. The
--- function returns `true` to broadcast, `false`/`nil` to suppress.
--- @class crap.LiveFilterContext
--- @field collection string Collection (or global) slug the mutation targets.
--- @field operation string The mutation operation: `"create"`, `"update"`, `"delete"`, `"undelete"`, `"unpublish"`, or `"restore"`.
--- @field data table<string, any> The document's field data for this event.
--- @field id string The affected document's id, exposed to Lua as `ctx.id` (consistent with the other hook contexts; the serialized event payload uses `document_id`).
--- @field edited_by? { id: string, email: string } The user who caused the mutation (`nil` for anonymous/system changes).
--- @field options? table Per-config options from the filter's `{ ref, options }` table; `nil` when configured as a bare ref string.

--- Which host surfaces a method can fire on. Surface filtering is
--- per-method: a method whose `surfaces` list omits the current
--- request's surface is skipped by the evaluator.
--- @alias crap.Surface
--- | "admin"
--- | "grpc"

--- Strategy is invoked on every request that passes the surface
--- filter. The strategy itself decides per-request whether to
--- authenticate. Emits a startup warning to make accidental
--- always-active strategies loud.
--- @class crap.ActivationAlways
--- @field always true Must be `true`. `false` is rejected at deserialize time.

--- Strategy fires only when the named header (lowercase, gRPC
--- metadata or HTTP header) is present on the request.
--- @class crap.ActivationHeader
--- @field header string Header name (lowercase) — e.g. `"x-api-key"`.

--- How a `Strategy` method is triggered for a given request.
---
--- `Always` is the explicit catch-all escape hatch (`mTLS`, multi-
--- signal strategies, `IdP` introspection). Required by the
--- validator to be the literal `{ always = true }` table —
--- `{ always = false }` is rejected at deserialize-time via the
--- custom `deserialize_true_only` helper. Use a `Header`
--- discriminator when the strategy fires on a specific request
--- header, which is the common case for API-key / SSO patterns.
--- @alias crap.Activation
--- | crap.ActivationAlways
--- | crap.ActivationHeader

--- Email + password login via the `Login` RPC. Issues a JWT
--- the `Bearer` method later validates. Owns the
--- password-only flags (`mfa`, `verify_email`,
--- `forgot_password`) so the password-only concerns aren't
--- scattered across the collection.
--- @class crap.AuthMethodPasswordLogin
--- @field type "password_login"
--- @field mfa? "email"|"custom"|false MFA mode. `"email"` sends the code by email, `"custom"` hands it to the `mfa_deliver` hook; `false` (or omit) disables.
--- @field mfa_when? string | crap.HookRef Optional Lua gate deciding WHETHER a verified login must complete the second factor — called after credential verification with `{ collection, user, surface, headers }`; return `false`/`nil` to skip MFA for this login, anything truthy to require it. Lets MFA apply per surface (`ctx.surface == "grpc"`) or per user field (`ctx.user.mfa_enabled`). Runs for any enabled MFA mode (`"email"` or `"custom"`); no hook = MFA always required. A hook error fails CLOSED (requires MFA).
--- @field mfa_deliver? string | crap.HookRef Delivery hook for `mfa = "custom"`: called after credential verification with `{ collection, user, code, expires_in }` — send the code via your channel (SMS, push, …). The code is SENSITIVE: never log it. Errors are logged server-side; the previously issued code (if any) stays valid. Required with `mfa = "custom"`, rejected otherwise (startup error).
--- @field verify_email? boolean Require email verification before login (default `false`).
--- @field forgot_password? boolean Enable the forgot-password flow (default `true`).

--- Accept JWTs in the standard `Authorization: Bearer …`
--- header / gRPC metadata. Default surfaces: all.
--- @class crap.AuthMethodBearer
--- @field type "bearer"
--- @field surfaces? crap.Surface[]|"all" Surfaces this method fires on (default: `{"admin", "grpc"}`).

--- Accept the `crap_session` cookie. Default surfaces: admin.
--- @class crap.AuthMethodSessionCookie
--- @field type "session_cookie"
--- @field surfaces? crap.Surface[]|"all" Surfaces this method fires on (default: `{"admin"}`).

--- Custom Lua-driven authentication. `authenticate` is a
--- `module.function`-shaped Lua hook ref. The hook receives
--- `{ headers, collection }` (matching today's contract) and
--- returns a user document or nil.
--- @class crap.AuthMethodStrategy
--- @field type "strategy"
--- @field name string Identifier used in logging + error messages. Doesn't have to be unique across collections, but should be.
--- @field authenticate string | crap.HookRef Lua function ref, e.g. `"hooks.auth.api_key"`. Receives `crap.AuthStrategyContext`; returns user doc or nil. May carry per-config options exposed to the hook as `ctx.options`.
--- @field activates_on crap.Activation Discriminator for when the strategy fires. See [`Activation`].
--- @field surfaces? crap.Surface[]|"all" Surfaces this method fires on (default: `{"admin"}`).

--- One authentication method on a collection. The collection's
--- `methods` list is ordered: the evaluator tries each in
--- declaration order, first match wins.
---
--- Serde shape: internally tagged on `type`, `snake_case`. Each
--- variant's fields appear at the same level as `type`.
---
--- ```json
--- { "type": "password_login", "mfa": "email", "verify_email": true }
--- { "type": "bearer", "surfaces": ["grpc", "admin"] }
--- { "type": "strategy", "name": "api-key", "authenticate": "hooks.auth.api_key",
---   "activates_on": { "header": "x-api-key" }, "surfaces": ["grpc"] }
--- ```
--- @alias crap.AuthMethod
--- | crap.AuthMethodPasswordLogin
--- | crap.AuthMethodBearer
--- | crap.AuthMethodSessionCookie
--- | crap.AuthMethodStrategy

--- Top-level authentication configuration for a collection.
---
--- `enabled = true` + non-empty `methods` makes this an auth
--- collection — provisions `_password_hash` and friends in the
--- schema, registers it as a target for `Login` / `Me` / etc.
---
--- `methods` is required (no implicit defaults). Use
--- `crap.auth.default_methods()` from Lua for the common
--- password+bearer+cookie set.
--- @class crap.Auth
--- @field enabled? boolean Enable auth for this collection. Required true when `methods` is non-empty.
--- @field token_expiry? integer JWT lifetime in seconds (default: 7200).
--- @field methods? crap.AuthMethod[] Ordered list of auth methods. Use `crap.auth.default_methods()` for the standard set or `crap.auth.with_defaults({...})` to extend it.

--- Resize fit mode for image processing.
--- @alias crap.ImageFit
--- | "cover"
--- | "contain"
--- | "inside"
--- | "fill"

--- A named image resize target (e.g. "thumbnail" at 200x200).
--- @class crap.ImageSize
--- @field name string Size name (e.g., "thumbnail", "card").
--- @field width integer Target width in pixels.
--- @field height integer Target height in pixels.
--- @field fit? crap.ImageFit Resize fit mode (default: "cover").

--- Encoding quality and processing mode for a single converted image format.
--- @class crap.FormatQuality
--- @field quality integer Encoding quality 1-100.
--- @field queue? boolean Defer conversion to the background image-processing queue instead of running synchronously during upload (default: false).

--- Auto-generate format variants for each upload size (e.g. WebP, AVIF).
--- @class crap.FormatOptions
--- @field webp? crap.FormatQuality Auto-generate WebP variant for each size.
--- @field avif? crap.FormatQuality Auto-generate AVIF variant for each size.

--- Per-collection upload configuration (MIME filtering, image sizes, format options).
--- @class crap.CollectionUpload
--- @field mime_types? string[] MIME type allowlist with glob support (e.g., "image/*"). Empty = any type.
--- @field max_file_size? integer|string Max file size — bytes (integer) or human-readable ("10MB", "1GB"). Overrides global default.
--- @field image_sizes? crap.ImageSize[] Resize definitions for image uploads.
--- @field admin_thumbnail? string Name of `image_size` to show in admin list.
--- @field format_options? crap.FormatOptions Auto-generate format variants for each size.

--- Configuration for document versioning and drafts on a collection.
--- @class crap.VersionsConfig
--- @field drafts? boolean Enable draft/publish workflow (default: false). Adds `_status` column.
--- @field max_versions? integer Maximum version snapshots to keep per document (default: unlimited).

--- Full definition of a collection, parsed from a Lua file. Maps to one `SQLite` table.
--- @class crap.CollectionConfig
--- @field labels? crap.Labels Display names.
--- @field timestamps? boolean Auto `created_at`/`updated_at` (default: true).
--- @field fields? crap.FieldDefinition[] Field definitions.
--- @field admin? crap.AdminConfig Admin UI options.
--- @field hooks? crap.Hooks Hook references.
--- @field auth? boolean | crap.Auth Enable authentication on this collection. `true` for defaults, or a config table with `strategies`/`token_expiry`/`disable_local`.
--- @field upload? boolean | crap.CollectionUpload Enable file uploads. `true` for defaults, or a config table with `mime_types`/`max_file_size`/`image_sizes`.
--- @field access? crap.Access Access control function refs.
--- @field mcp? crap.McpCollectionConfig MCP tool description and options.
--- @field live? boolean | string Live event broadcasting. `false` to disable, string for Lua function ref that receives `{ collection, operation, data }` and returns boolean. Absent = enabled (broadcast all).
--- @field versions? boolean | crap.VersionsConfig Enable versioning. `true` for defaults, or a config table with `drafts`/`max_versions`.
--- @field indexes? crap.IndexDefinition[] Compound indexes (multi-column). Created on startup, stale indexes dropped.
--- @field soft_delete? boolean Enable soft deletes (move to trash instead of permanent deletion). When `true`, `delete` moves documents to trash; the row is purged later according to `soft_delete_retention`.
--- @field soft_delete_retention? string Retention period for soft-deleted documents before auto-purge. Human-readable duration: `"30d"`, `"7d"`, `"90d"`. `nil` = manual purge only. Only relevant when `soft_delete = true`.
--- @field required_locales? "all" | string[] Default `required_locales` for this collection's localized required fields — applies to any field that doesn't set its own. `"all"` = every configured locale; a list names specific locales. Unset → only the default locale is required.

--- A compound index definition (multi-column, optionally unique).
--- @class crap.IndexDefinition
--- @field fields string[] Column names to include in the index (required).
--- @field unique? boolean Create a UNIQUE index (default: false).


--- Typegen-only mirror of [`Access`] for globals. A global is a single row with
--- only `get`/`update` operations, so it rejects `create`, `delete`, and `trash`
--- at config load. This restricted shape gives the Lua LSP the correct key set
--- (`read` / `draft` / `update` / `versions`) on `crap.GlobalConfig.access`
--- instead of advertising collection-only keys that would fail at parse. The
--- runtime field is still an [`Access`]; this struct is never constructed — only
--- its derived `render_lua_annotation` is used, to emit the `crap.GlobalAccess`
--- class into `types/crap.lua`. Keep its fields in sync with the matching
--- [`Access`] fields.
--- @class crap.GlobalAccess
--- @field read? string | crap.HookRef Hook ref for read (published) access control.
--- @field draft? string | crap.HookRef Hook ref for reading draft (unpublished) content. Falls back to `update` when unset, so previewing a draft requires edit-level access by default.
--- @field update? string | crap.HookRef Hook ref for update access control.
--- @field versions? string | crap.HookRef Hook ref restricting version-history visibility — a *toggle*, not a per-snapshot filter. Falls back to `update` when unset (a published-only reader sees no history by default); returning a filter table is rejected (snapshot scoping follows `read`/`draft`).
--- @field admin? string | crap.HookRef Hook ref gating whether the global is visible/usable in the **admin UI**. A boolean rule; unset means visible (permissive default). Filter table is a configuration error.
--- @field mcp? string | crap.HookRef Hook ref gating whether the global is exposed to the **MCP** surface. A boolean rule; unset means exposed (permissive default). Filter table is a configuration error.

--- Global definitions are simpler — single-document collections.
--- @class crap.GlobalConfig
--- @field labels? crap.Labels Display names.
--- @field fields? crap.FieldDefinition[] Field definitions.
--- @field hooks? crap.Hooks Hook references.
--- @field access? crap.GlobalAccess Access control function refs. Globals support only `read`, `draft`, `update`, and the `versions` toggle — `create`/`delete`/`trash` are rejected at config load (a global is a single row with `get`/`update` operations only).
--- @field mcp? crap.McpCollectionConfig MCP tool description and options.
--- @field live? boolean | string Live event broadcasting. Same as collection `live`.
--- @field versions? boolean | crap.VersionsConfig Enable versioning. Same as collection `versions`.

--- Pagination metadata for a find result.
--- @class crap.PaginationInfo
--- @field total_docs integer Total matching documents (before limit/page).
--- @field limit integer Applied limit for this query.
--- @field has_next_page boolean Whether a next page exists.
--- @field has_prev_page boolean Whether a previous page exists.
--- @field total_pages? integer Total number of pages (offset mode only).
--- @field page? integer Current page number (offset mode only, 1-based).
--- @field page_start? integer 1-based index of the first document on the current page (offset mode only).
--- @field prev_page? integer Previous page number (offset mode only, nil if on first page).
--- @field next_page? integer Next page number (offset mode only, nil if on last page).
--- @field start_cursor? string Opaque cursor of the first document in results (cursor mode only).
--- @field end_cursor? string Opaque cursor of the last document in results (cursor mode only).

--- A single content document with an ID, user-defined fields, and optional timestamps.
---
--- **Type-emission note:** the `LuaLS` class deliberately does NOT carry an
--- `[string] any` index signature. Adding one caused `LuaLS` to collapse
--- typed subclasses (`crap.doc.Posts : crap.Document`, etc.) back into
--- the permissive base — hover popups showed `crap.Document` even when
--- the per-collection narrowing was correct, because an index signature
--- of `any` subsumes the structural extension. Subclasses now expose
--- their typed fields cleanly; for the dynamic-slug `find()` fallback
--- (returning the bare `crap.FindResult`), users either cast with
--- `--[[@as crap.doc.X]]` or index via `doc["field"]` if they need to
--- reach a field outside the base shape.
--- @class crap.Document
--- @field id string Unique document ID (nanoid).
--- @field created_at? string ISO 8601 timestamp (if `timestamps` enabled on the collection).
--- @field updated_at? string ISO 8601 timestamp (if `timestamps` enabled on the collection).

--- Scalar filter value — `string`, `integer`, `number`, or `boolean`.
--- Modeled as an untagged Rust enum so a Lua string and a Lua number
--- both deserialize into the right variant without any per-field
--- hint. The Lua alias is emitted as a type-union derived from the
--- variant payload types (`boolean | integer | number | string`).
--- @alias crap.FilterScalar boolean | integer | number | string

--- Filter operator table. Use one key per operator on the operator
--- side of a `where = { field = { … } }` entry. Simple string /
--- number / boolean values on the right-hand side are treated as
--- `equals` automatically (see `FilterValue::Scalar`).
--- @class crap.FilterOperators
--- @field equals? crap.FilterScalar Exact match (`field = value`).
--- @field not_equals? crap.FilterScalar Not equal (`field != value`).
--- @field like? string SQL `LIKE` pattern (`field LIKE value`).
--- @field contains? string Substring match (`field LIKE %value%`).
--- @field greater_than? crap.FilterScalar Greater than (`field > value`).
--- @field less_than? crap.FilterScalar Less than (`field < value`).
--- @field greater_than_or_equal? crap.FilterScalar Greater than or equal (`field >= value`).
--- @field less_than_or_equal? crap.FilterScalar Less than or equal (`field <= value`).
--- @field ["in"]? crap.FilterScalar[] Value in list (`field IN (...)`).
--- @field not_in? crap.FilterScalar[] Value not in list (`field NOT IN (...)`).
--- @field exists? boolean Field is not null (`IS NOT NULL`). Only `true` is accepted — `false` is an error.
--- @field not_exists? boolean Field is null (`IS NULL`). Only `true` is accepted — `false` is an error.

--- One filter value in a `where` clause: scalar (treated as
--- `equals`) or operator table.
--- @alias crap.FilterValue crap.FilterScalar | crap.FilterOperators

--- One AND-group inside a `where.or` array. The Lua user passes a
--- map of `field → FilterValue` exactly like the top-level `where`
--- clause, and the OR clause is `{ group1, group2, … }`. Documented
--- as a type alias so the `where` type union (on each `*QueryInput`)
--- can name it.
--- @alias crap.OrCondition table<string, crap.FilterValue>

--- Query passed to `crap.collections.find(collection, query)`.
--- @class crap.FindQuery
--- @field where? table<string, crap.FilterValue | crap.OrCondition[]> Field filters. String values = `equals`, table values = operators. Use `["or"]` for OR groups. Keys support dot notation for nested fields (`"seo.title"`, `"variants.color"`, `"tags.id"`).
--- @field order_by? string Sort field (prefix with `"-"` for descending).
--- @field limit? integer Max results to return.
--- @field page? integer Page number (1-based). Converted to offset internally.
--- @field offset? integer Number of results to skip (raw row offset, for batch iteration). Ignored when `page` is set — `page` takes precedence.
--- @field depth? integer Population depth for relationship fields (default: `[depth] default_depth` from `crap.toml`, which defaults to 1).
--- @field locale? string Locale code for localized fields (`"en"`, `"de"`, `"all"`).
--- @field select? string[] Fields to return. Nil/empty = all fields.
--- @field draft? boolean Include draft documents (versioned collections only).
--- @field override_access? boolean Skip access control checks (default: `false`).
--- @field trash? boolean Include soft-deleted documents (trash listings).
--- @field after_cursor? string Forward cursor token (from previous response's `end_cursor`).
--- @field before_cursor? string Backward cursor token (from previous response's `start_cursor`).
--- @field search? string FTS5 full-text search query.

--- Result of `crap.collections.find(...)`. Constructed by the handler
--- and serialized via `LuaSerdeExt::to_value` — the Lua-side
--- `crap.FindResult` class is derived from this struct.
--- @class crap.FindResult
--- @field documents crap.Document[] Matching documents.
--- @field pagination crap.PaginationInfo Pagination metadata.

--- Lua function references for field-level access control (read/create/update).
--- @class crap.FieldAccess
--- @field read? string | crap.HookRef Hook ref for field read access control.
--- @field create? string | crap.HookRef Hook ref for field create access control.
--- @field update? string | crap.HookRef Hook ref for field update access control.

--- Context passed to hook functions.
---
--- `data` is mutable in `before_*` hooks; read-only in `after_*` hooks.
--- `hook_depth` (not on the Rust struct, added at `to_lua_table` time
--- from `HookDepth` app-data) tracks recursion depth — `0` for a
--- top-level API call, `1+` from Lua CRUD invoked inside another hook.
--- @class crap.HookContext
--- @field collection string Collection slug.
--- @field operation "create"|"update"|"delete"|"find"|"find_by_id"|"get"|"init" The operation being performed.
--- @field data table<string, any> Document data. For read hooks, contains document fields including `id` / timestamps. For `before_delete` / `after_delete` hooks, contains the deleted document's fields plus `id` (and `soft_delete` for a soft delete) — so a hook can inspect what is being removed; a hard delete leaves no row to re-fetch, so `after_delete` relies on this snapshot. In `after_change` hooks, `data.id` carries the new document ID.
--- @field locale? string The content locale this operation targets (e.g. `"en"`, `"de"`) — the requested locale, or the default locale when none was given. Nil when localization is disabled (and on the locale-agnostic `before_delete` / `after_delete` hooks, which remove the whole row across all locales). Otherwise the same resolved value every hook surface sees (field hooks, validators, access functions).
--- @field draft? boolean `true` when this is a draft save (only set for collections with `versions.drafts` enabled).
--- @field context table<string, any> Operation-scoped shared table that persists from `before_validate` through `after_change` within one write operation (or `before_read` → `after_read` for one read) — NOT across the whole HTTP request. Only JSON-compatible values survive (no functions / userdata).
--- @field user? table Authenticated user document (nil if unauthenticated or no auth collection).
--- @field ui_locale? string Admin UI locale code (e.g., `"en"`, `"de"`). Nil if not set or called from gRPC without locale context.
--- @field id? string The id of the document this event targets, exposed to Lua as `ctx.id` (matching the field-hook, validator, and access contexts). Populated across the write lifecycle — `update`/`delete` before- and after-hooks, `after_change` on create (the freshly assigned id; `nil` in create's before-hooks, where no row exists yet), `after_read`, and `"default"` for globals. Also set on live-broadcast hooks (`before_broadcast`). (The Rust field stays `document_id`; only the Lua-facing key is `id`.)
--- @field edited_by? { id: string, email: string } The user who caused a live-broadcast mutation. Set on `before_broadcast`; `nil` elsewhere or for anonymous changes. (Distinct from `user`, the caller of a request — broadcast fires post-commit with no request user.)
--- @field hook_depth integer  Current recursion depth. `0` = top-level API/admin call, `1+` = from Lua CRUD inside hooks. Hooks are skipped when this reaches `hooks.max_depth` (default: `3`).
--- @field options? table  Per-config options from this hook ref's `{ ref, options }` table; `nil` when the hook was configured as a bare ref string.

--- The authenticated user attached to `crap.AccessContext.user`.
--- Carries the base `crap.Document` shape (id + timestamps) plus an
--- index signature for arbitrary field access — the static type
--- can't narrow further because a project can have multiple auth
--- collections. Cast explicitly when you know the collection:
--- `local u = context.user --[[@as crap.doc.Users]]`.
--- @class crap.AuthUser : crap.Document
--- @field [string] any

--- Context passed to collection- and field-level access functions.
--- Return `true` to allow, `false` / `nil` to deny, or a filter table
--- (read only) to allow with query constraints.
--- @class crap.AccessContext
--- @field user? crap.AuthUser Full user document from the auth collection (nil if anonymous). Typed as `crap.AuthUser` (a `crap.Document` variant with an `[string] any` index signature) so access functions can read `context.user.role` / `context.user.email` etc. without per-call casts — the static type can't narrow to a specific auth-collection doc since projects may have multiple auth collections. Users who know their auth collection can still cast: `local u = context.user --[[@as crap.doc.Users]]`.
--- @field id? string Document ID (for `update` / `delete` / `find_by_id`).
--- @field data? table<string, any> The data for this check. - **Collection access:** the **incoming** data for `create` / `update` (the submitted change, *not* the stored row), `nil` for reads/deletes. To gate on existing persisted values, return a **filter table** instead of a boolean (e.g. `return { author_id = ctx.user.id }`). - **Field access:** the field's **immediate level** — the row object for a field inside an array/blocks row, the group object for a field in a group, the whole document at top level. Same meaning as `ctx.data` in a field lifecycle hook, so a field rule can gate on adjacent (sibling) values — e.g. lock a field unless `ctx.data.kind == "advanced"`. Present on reads too (the level being read).
--- @field document? table<string, any> **Field access only.** The full document the field belongs to (the stored document on a read/`update`; the full incoming document on `create`), stable as the check descends into array/blocks rows — mirrors `ctx.document` in a field lifecycle hook. Lets a field rule depend on values outside its own level, e.g. make a field read-only unless `ctx.document.status == "published"`. `nil` for collection-level checks (use a filter table there instead).
--- @field locale? string The locale this operation targets, when localization is enabled — the requested locale, or the default locale when none was specified. `nil` when localization is disabled. Lets access functions enforce per-locale rules, e.g. restrict a user to certain locales or lock a field to the default locale. Also `nil` when the access function is invoked outside a single-locale operation (e.g. manually via `crap.access.check`, or a nested join read-access check) — gate defensively (`if ctx.locale and ... then`).
--- @field operation string The operation triggering this check: `"create"`, `"update"`, `"delete"`, `"trash"` (soft delete), `"undelete"`, `"unpublish"`, `"restore"`, `"find"`, `"find_by_id"`, `"count"`, `"search"`, `"get"` (global read), `"read"` (admin read-gating: nav, back-references, condition eval, upload serve), `"subscribe"`, … Lets one shared access function branch on the operation instead of registering a separate function per operation.
--- @field collection string The collection (or job) slug this check is for — so a function reused across collections can tell which one it is gating.
--- @field ui_locale? string Admin UI locale code (e.g. `"en"`, `"de"`) when the check originates from an admin request; `nil` otherwise (gRPC/REST/internal checks). Distinct from `locale` (the content locale) — this is the operator's UI language.
--- @field options? table Per-config options from the hook ref's `{ ref, options }` table; `nil` when the access rule was configured as a bare ref string.

-- ── Function-type aliases (one-liner `---@type` for callables) ─────

--- Generic collection hook. Use for hooks that take `crap.HookContext`
--- (no per-collection narrowing); for typed contexts use
--- `crap.hook_fn.<Pascal>` from `hooks.lua`.
--- @alias crap.hook_fn fun(ctx: crap.HookContext): crap.HookContext

--- Generic field hook. Use for hooks that take the generic
--- `crap.FieldHookContext`; for typed contexts use
--- `crap.field_hook_fn.<Pascal>` from `hooks.lua`. Same shape
--- as the older `crap.FieldHookFn` alias — both are kept; new code
--- should prefer this lowercase name for naming consistency with
--- the per-collection variants.
--- @alias crap.field_hook_fn fun(value: any, context: crap.FieldHookContext): any

--- Access control function. Returns `true`/`false` for global allow /
--- deny, or a filter table to constrain results.
--- @alias crap.access_fn fun(ctx: crap.AccessContext): boolean | table

--- Custom auth strategy `authenticate` callback. Receives request
--- headers + the collection slug; returns a user document on success
--- or `nil` to fall through to the next method.
--- @alias crap.auth_strategy_fn fun(ctx: crap.AuthStrategyContext): crap.Document?
--- @alias crap.mfa_when_fn fun(ctx: crap.MfaWhenContext): boolean?
--- @alias crap.mfa_deliver_fn fun(ctx: crap.MfaDeliverContext)

--- Job handler entry point. The runtime ignores the return value.
--- @alias crap.job_handler_fn fun(ctx: crap.JobHandlerContext)

--- Computed row label for arrays/blocks. Receives the row table and
--- returns the display string (or `nil` to fall back to `label_field`).
--- @alias crap.row_label_fn fun(row: table<string, any>): string?

-- ── crap.any — cross-collection typing helpers ─────────────────────

--- Typing helpers for callables that don't belong to a specific
--- collection (generic-context collection/field hooks, access
--- functions, auth strategies, job handlers, row labels). Each
--- function is a no-op pass-through at runtime; its only purpose is
--- to give LuaLS a typed parameter slot so the body's `value` / `ctx`
--- params get inferred via `Lua.type.inferParamType`.
---
--- For per-collection narrowing use the slug accessors instead:
--- `crap.collections.<slug>.{hook,field_hook,condition}` /
--- `crap.globals.<slug>.{hook,field_hook,condition}`.
--- @class crap.any
crap.any = {}

--- Wrap a collection hook that takes a generic `crap.HookContext`.
--- @param fn crap.hook_fn
--- @return crap.hook_fn
function crap.any.collection_hook(fn) end

--- Wrap a field hook that takes a generic `crap.FieldHookContext`.
--- @param fn crap.field_hook_fn
--- @return crap.field_hook_fn
function crap.any.field_hook(fn) end

--- Wrap an access control function.
--- @param fn crap.access_fn
--- @return crap.access_fn
function crap.any.access(fn) end

--- Wrap an auth strategy `authenticate` callback.
--- @param fn crap.auth_strategy_fn
--- @return crap.auth_strategy_fn
function crap.any.auth_strategy(fn) end

--- Wrap a job handler.
--- @param fn crap.job_handler_fn
--- @return crap.job_handler_fn
function crap.any.job_handler(fn) end

--- Wrap a custom-route handler (so `ctx` types as `crap.RouteContext`).
--- @param fn crap.route_handler_fn
--- @return crap.route_handler_fn
function crap.any.route_handler(fn) end

--- Wrap a row-label function for arrays/blocks.
--- @param fn crap.row_label_fn
--- @return crap.row_label_fn
function crap.any.row_label(fn) end

--- Wrap a generic display condition (no per-collection data narrowing).
--- @param fn fun(data: table<string, any>, ctx: crap.ConditionContext): boolean | table
--- @return fun(data: table<string, any>, ctx: crap.ConditionContext): boolean | table
function crap.any.display_condition(fn) end

-- ── crap.collections ─────────────────────────────────────────

--- Collection definition and runtime CRUD operations.
--- @class crap.collections
crap.collections = {}

--- Define a new collection. Call this in collection definition files.
--- @param slug string  Unique collection identifier (used in URLs and DB).
--- @param config crap.CollectionConfig  Collection configuration.
function crap.collections.define(slug, config) end

-- ── crap.collections.config ──────────────────────────────────

--- Schema introspection sub-table for collections.
--- @class crap.collections.config
crap.collections.config = {}

--- Get a collection's current definition as a Lua table.
--- Returns the full config compatible with `define()` for round-trip editing.
--- @param slug string  Collection slug.
--- @return crap.CollectionConfig? # The collection config, or nil if not found.
function crap.collections.config.get(slug) end

--- List all registered collections as a slug-keyed table of full configs.
--- Iterate with `for slug, def in pairs(crap.collections.config.list()) do ... end`.
--- @return table<string, crap.CollectionConfig> # Slug -> collection config map.
function crap.collections.config.list() end

--- Optional options for `crap.collections.find_by_id`.
--- @class crap.FindByIdOptions
--- @field depth? integer Population depth for relationship fields. Unset uses the configured `[depth] default_depth` (matching the gRPC/MCP surfaces); `0` = return IDs only. Clamped to the configured `[depth] max_depth`.
--- @field locale? string Locale code for localized fields (e.g., `"en"`, `"de"`, `"all"`). Nil = default locale.
--- @field select? string[] Fields to return. Nil or empty = all fields. `id` is always included.
--- @field draft? boolean When `true` and the collection has `versions.drafts`, returns the latest draft version snapshot instead of the published main-table data.
--- @field trash? boolean When `true` and the collection has `soft_delete`, looks up the document among soft-deleted (trash) rows instead of live ones.
--- @field override_access? boolean Skip access control checks (default: `false`). Set to `true` in trusted internal code to bypass collection-level and field-level access for the current user.

--- Optional options for `crap.collections.create`.
--- @class crap.CreateOptions
--- @field locale? string Locale code for localized fields. Nil = default locale.
--- @field override_access? boolean Skip access control checks (default: `false`). Set to `true` in trusted internal code to bypass collection-level and field-level access for the current user.
--- @field draft? boolean When `true` and the collection has `versions.drafts`, creates the document with `_status = 'draft'` and skips required-field validation.
--- @field hooks? boolean Run lifecycle hooks (default: `true`). Set `false` to bypass hooks (e.g., for seeding/migrations).
--- @field events? boolean Emit a live-update event for the created document (default: `true`). Set `false` for a quiet write (e.g., seeding/migrations).

--- Create a new document.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param data table<string, any>  Field values.
--- @param opts crap.CreateOptions?  Optional options (e.g., `{ locale = "de" }`).
--- @return crap.Document
function crap.collections.create(collection, data, opts) end

--- Optional options for `crap.collections.update`.
--- @class crap.UpdateOptions
--- @field locale? string Locale code for localized fields. Nil = default locale.
--- @field override_access? boolean Skip access control checks (default: `false`). Set to `true` in trusted internal code to bypass collection-level and field-level access for the current user.
--- @field draft? boolean When `true` and the collection has `versions.drafts`, performs a version-only save (main table unchanged, only a draft version snapshot is created).
--- @field hooks? boolean Run lifecycle hooks (default: `true`). Set `false` to bypass hooks.
--- @field unpublish? boolean When `true`, sets `_status` to `"draft"` (unpublishes). Data is not modified. Requires `versions` on the collection — errors otherwise.
--- @field events? boolean Emit a live-update event for the updated document (default: `true`). Set `false` for a quiet write.

--- Update an existing document.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param id string  Document ID.
--- @param data table<string, any>  Fields to update (partial).
--- @param opts crap.UpdateOptions?  Optional options (e.g., `{ locale = "de" }`).
--- @return crap.Document
function crap.collections.update(collection, id, data, opts) end

--- Optional options for `crap.collections.delete`.
--- @class crap.DeleteOptions
--- @field override_access? boolean Skip access control checks (default: `false`). Set to `true` in trusted internal code to bypass collection-level access for the current user.
--- @field hooks? boolean Run lifecycle hooks (default: `true`). Set `false` to bypass hooks.
--- @field force_hard_delete? boolean Bypass `soft_delete` and remove the row permanently. Mirrors the same flag on the gRPC/HTTP delete handlers.
--- @field events? boolean Emit a live-update event for the deleted document (default: `true`). Set `false` for a quiet delete.

--- Delete a document.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param id string  Document ID.
--- @param opts crap.DeleteOptions?  Optional options.
--- @return boolean # True on successful delete.
function crap.collections.delete(collection, id, opts) end

--- Optional options for `crap.collections.unpublish`.
--- @class crap.UnpublishOptions
--- @field override_access? boolean Skip access control checks (default: `false`).
--- @field hooks? boolean Run lifecycle hooks (default: `true`).
--- @field events? boolean Emit a live-update event for this change (default: `true`). Parity with `crap.collections.update{ unpublish = true, events = ... }`.

--- Unpublish a document — sets `_status` to `"draft"` without modifying
--- the underlying field data. Only available on collections with
--- `versions` enabled.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param id string  Document ID.
--- @param opts crap.UnpublishOptions?  Optional options.
--- @return crap.Document
function crap.collections.unpublish(collection, id, opts) end

--- Optional options for `crap.collections.undelete`.
--- @class crap.UndeleteOptions
--- @field override_access? boolean Skip access control checks (default: `false`).
--- @field events? boolean Emit a live-update event for the restored document (default: `true`). Set `false` for a quiet restore. Parity with the gRPC/MCP undelete.

--- Restore a soft-deleted document. Only available on collections with
--- `soft_delete` enabled.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param id string  Document ID.
--- @param opts crap.UndeleteOptions?  Optional options.
--- @return boolean # True when the document was successfully restored.
function crap.collections.undelete(collection, id, opts) end

--- Optional options for `crap.collections.validate`.
--- @class crap.ValidateOptions
--- @field locale? string Locale code for localized fields. Nil = default locale.
--- @field override_access? boolean Skip access control checks (default: `false`).
--- @field draft? boolean Validate as a draft (relaxes required-field checks the same way as `crap.collections.create({ draft = true })`).
--- @field id? string Existing document ID to exclude from uniqueness checks (set when validating an update).

--- Result of `crap.collections.validate(...)`. `errors` is present only
--- when `valid = false`.
--- @class crap.ValidateResult
--- @field valid boolean True when the input passes validation.
--- @field errors? table<string, string> Map of field name → error message. Present only when `valid = false`.

--- Validate document data without persisting. Returns
--- `{ valid = true }` on success, or `{ valid = false, errors = {...} }`
--- on validation failure (errors keyed by field name).
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param data table<string, any>  Field values to validate.
--- @param opts crap.ValidateOptions?  Optional options.
--- @return crap.ValidateResult
function crap.collections.validate(collection, data, opts) end

--- Query passed to `crap.collections.count(collection, query)`.
--- @class crap.CountQuery
--- @field where? table<string, crap.FilterValue | crap.OrCondition[]>
--- @field locale? string Locale code for localized fields.
--- @field override_access? boolean Skip access control checks (default: `false`).
--- @field draft? boolean Include draft documents (default: `false`).
--- @field trash? boolean Count only soft-deleted (trash) documents (default: `false`).
--- @field search? string FTS5 full-text search query.

--- Count documents matching a query.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param query crap.CountQuery?  Optional query parameters.
--- @return integer # Number of matching documents.
function crap.collections.count(collection, query) end

--- Query passed to `crap.collections.update_many(collection, query, data, opts?)`.
--- @class crap.UpdateManyQuery
--- @field where? table<string, crap.FilterValue | crap.OrCondition[]>

--- Query passed to `crap.collections.delete_many(collection, query, opts?)`.
--- @class crap.DeleteManyQuery
--- @field where? table<string, crap.FilterValue | crap.OrCondition[]>

--- Optional options for `crap.collections.create_many`. Standalone from the
--- single-create options so the bulk-only `events` default (`false`) is
--- explicit.
--- @class crap.CreateManyOptions
--- @field override_access? boolean Skip access control checks (default: `false`).
--- @field draft? boolean Create documents as drafts (default: `false`).
--- @field locale? string Locale code for localized field writes (default: default locale).
--- @field hooks? boolean Run lifecycle hooks (default: `true`). Set `false` to bypass.
--- @field events? boolean Emit a live-update event per created document (default: `false` — bulk operations are quiet). Set `true` to notify subscribers.

--- Optional options for `crap.collections.update_many`. Standalone from the
--- single-update options so the bulk-only `events` default (`false`) is
--- explicit (and bulk-irrelevant keys like `unpublish` are not accepted).
--- @class crap.UpdateManyOptions
--- @field locale? string Locale code for localized fields. Nil = default locale.
--- @field override_access? boolean Skip access control checks (default: `false`).
--- @field draft? boolean Apply changes to draft versions (default: `false`).
--- @field hooks? boolean Run lifecycle hooks (default: `true`). Set `false` to bypass.
--- @field events? boolean Emit a live-update event per modified document (default: `false` — bulk operations are quiet). Set `true` to notify subscribers.

--- Optional options for `crap.collections.delete_many`. Standalone from the
--- single-delete options so the bulk-only `events` default (`false`) is
--- explicit.
--- @class crap.DeleteManyOptions
--- @field locale? string Locale code. Validated but not used for matching (`delete_many` spans locales). Nil = default locale.
--- @field override_access? boolean Skip access control checks (default: `false`).
--- @field hooks? boolean Run lifecycle hooks (default: `true`). Set `false` to bypass.
--- @field force_hard_delete? boolean Bypass `soft_delete` and remove rows permanently (default: `false`).
--- @field trash? boolean Target already-trashed documents and permanently remove them (empty the trash). Implies a hard delete gated by `access.delete`; matches only rows with `_deleted_at` set (default: `false`).
--- @field events? boolean Emit a live-update event per deleted document (default: `false` — bulk operations are quiet). Set `true` to notify subscribers.

--- Result of a bulk update operation.
--- @class crap.UpdateManyResult
--- @field modified integer Number of documents updated.

--- Result of `crap.collections.delete_many(...)`. `deleted` is the sum
--- of `hard_deleted + soft_deleted` from
--- `service::collections::DeleteManyResult` — the Lua user doesn't care
--- about that distinction.
--- @class crap.DeleteManyResult
--- @field deleted integer Number of documents deleted (hard + soft).
--- @field skipped integer Number of documents skipped because they had incoming references.

--- Result of a bulk create operation.
--- @class crap.CreateManyResult
--- @field created integer Number of documents created.
--- @field documents crap.Document[] The created documents in order.

--- One row of `crap.collections.list_versions(...)`. Projection of
--- `core::document::VersionSnapshot` — drops the full `snapshot` payload
--- and the `parent` ID since the Lua user already supplied the parent.
--- @class crap.VersionSummary
--- @field id string Version row ID (not the parent document ID).
--- @field version integer Monotonically-increasing version number, newest first.
--- @field status string Version status (e.g. `"published"`, `"draft"`).
--- @field latest boolean `true` for the newest version, `false` otherwise.
--- @field created_at? string ISO 8601 timestamp the snapshot was taken (if recorded).

--- Result of `crap.collections.list_versions(...)`.
--- @class crap.ListVersionsResult
--- @field documents crap.VersionSummary[] Versions, newest first. Named `documents` for consistency with `find` / `create_many` (was `docs`).
--- @field pagination crap.PaginationInfo Pagination metadata.

--- Bulk create multiple documents from an array of data tables.
--- All-or-nothing: a single transaction wraps every insert.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param items table<string, any>[]  Array of field-value tables — one per document.
--- @param opts crap.CreateManyOptions?  Optional options applied to every document.
--- @return crap.CreateManyResult
function crap.collections.create_many(collection, items, opts) end

--- Update multiple documents matching a query. All-or-nothing: checks
--- update access for every matched document first; if any fails, returns
--- an error and nothing is modified. Runs the full per-document lifecycle
--- (`before_validate`, field validation, `before_change`, `after_change`)
--- by default; pass `hooks = false` in opts to skip hooks and validation for
--- large batches. Inside hooks, runs within the parent operation's
--- transaction.
--- @param collection string  Collection slug.
--- @param query crap.UpdateManyQuery  Query to match documents.
--- @param data table<string, any>  Fields to update on all matched documents.
--- @param opts crap.UpdateManyOptions?  Optional options.
--- @return crap.UpdateManyResult
function crap.collections.update_many(collection, query, data, opts) end

--- Delete multiple documents matching a query. All-or-nothing: checks
--- delete access for every matched document first; if any fails, returns
--- an error and nothing is modified. Fires per-document delete hooks
--- (`before_delete`, `after_delete`) by default; pass `hooks = false` in opts
--- to skip them for large batches. Referenced documents are skipped
--- (counted in `skipped`), not errored. Inside hooks, runs within the
--- parent operation's transaction.
--- @param collection string  Collection slug.
--- @param query crap.DeleteManyQuery  Query to match documents.
--- @param opts crap.DeleteManyOptions?  Optional options.
--- @return crap.DeleteManyResult
function crap.collections.delete_many(collection, query, opts) end

--- Optional options for `crap.collections.list_versions`.
--- @class crap.ListVersionsOptions
--- @field limit? integer Max number of versions to return.
--- @field offset? integer Offset for pagination.
--- @field override_access? boolean Skip access control checks (default: `false`). Set to `true` in trusted internal code (jobs, migrations) to bypass collection-level read-access checks.

--- List version snapshots for a document, newest first. Returns a table
--- of [`crap.VersionSummary`](lua://crap.VersionSummary) rows plus
--- pagination metadata. Only available on collections with `versions`
--- enabled.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param id string  Parent document ID.
--- @param opts crap.ListVersionsOptions?  Optional options.
--- @return crap.ListVersionsResult
function crap.collections.list_versions(collection, id, opts) end

--- Optional options for `crap.collections.restore_version`.
--- @class crap.RestoreVersionOptions
--- @field override_access? boolean Skip access control checks (default: `false`). Set to `true` in trusted internal code (jobs, migrations) to bypass collection-level access checks.

--- Restore a previous version: copies the snapshot data back onto the
--- parent document and writes a new version row. Returns the restored
--- document. Only available on collections with `versions` enabled.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param id string  Parent document ID.
--- @param version_id string  Version row ID to restore.
--- @param opts crap.RestoreVersionOptions?  Optional options.
--- @return crap.Document
function crap.collections.restore_version(collection, id, version_id, opts) end

--- Return the incoming-reference count for a document. Used to gauge
--- whether deletion would be blocked by relationships from other
--- collections.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param id string  Document ID.
--- @return integer # Number of incoming references.
function crap.collections.ref_count(collection, id) end

-- ── crap.globals ─────────────────────────────────────────────

--- Global (singleton document) definition and runtime operations.
--- @class crap.globals
crap.globals = {}

--- Define a new global. Call this in global definition files.
--- @param slug string  Unique global identifier.
--- @param config crap.GlobalConfig  Global configuration.
function crap.globals.define(slug, config) end

-- ── crap.globals.config ──────────────────────────────────────

--- Schema introspection sub-table for globals.
--- @class crap.globals.config
crap.globals.config = {}

--- Get a global's current definition as a Lua table.
--- Returns the full config compatible with `define()` for round-trip editing.
--- @param slug string  Global slug.
--- @return crap.GlobalConfig? # The global config, or nil if not found.
function crap.globals.config.get(slug) end

--- List all registered globals as a slug-keyed table of full configs.
--- Iterate with `for slug, def in pairs(crap.globals.config.list()) do ... end`.
--- @return table<string, crap.GlobalConfig> # Slug -> global config map.
function crap.globals.config.list() end

--- Optional options for `crap.globals.get`.
--- @class crap.GlobalGetOptions
--- @field locale? string Locale code for localized fields. Nil = default locale.
--- @field override_access? boolean Skip access control checks (default: `false`). Set to `true` in trusted internal code to bypass the global's read access function.
--- @field draft? boolean Include unpublished (draft) content (default: `false`). When the global has drafts enabled and has been unpublished, a normal read serves the last published snapshot; set this to `true` to read the draft instead.

--- Optional options for `crap.globals.update`.
--- @class crap.GlobalUpdateOptions
--- @field locale? string Locale code for localized fields. Nil = default locale.
--- @field override_access? boolean Skip access control checks (default: `false`). Set to `true` in trusted internal code to bypass the global's update access function.
--- @field hooks? boolean Run lifecycle hooks (default: `true`). Set false to bypass hooks (e.g., for seeding/migrations).
--- @field draft? boolean When `true` and the global has `versions.drafts`, performs a version-only save (main row unchanged, only a draft version snapshot is created). Matches `crap.collections.update`'s `draft`.
--- @field events? boolean Emit a live-update event for the updated global (default: `true`). Set `false` for a quiet write.

--- Optional options for `crap.globals.unpublish`.
--- @class crap.GlobalUnpublishOptions
--- @field override_access? boolean Skip access control checks (default: `false`).
--- @field hooks? boolean Run lifecycle hooks (default: `true`).

--- Optional options for `crap.globals.validate`. Globals are a singleton
--- row, so there is no `id` to exclude — validation always runs in update
--- mode against the fixed `default` row.
--- @class crap.GlobalValidateOptions
--- @field locale? string Locale code for localized fields. Nil = default locale.
--- @field override_access? boolean Skip access control checks (default: `false`).
--- @field draft? boolean Validate as a draft (relaxes required-field checks for globals with drafts enabled).

--- Get a global's current value.
--- @param slug string  Global slug.
--- @param opts crap.GlobalGetOptions?  Optional options (e.g., `{ locale = "de" }`).
--- @return crap.Document
function crap.globals.get(slug, opts) end

--- Update a global's value.
--- @param slug string  Global slug.
--- @param data_table table<string, any>  Fields to update.
--- @param opts crap.GlobalUpdateOptions?  Optional options (e.g., `{ locale = "de" }`).
--- @return crap.Document
function crap.globals.update(slug, data_table, opts) end

--- Unpublish a global — sets `_status` to `"draft"` without modifying the
--- stored field data. Only available on globals with `versions` enabled.
--- Inside hooks, runs within the parent operation's transaction.
--- @param slug string  Global slug.
--- @param opts crap.GlobalUnpublishOptions?  Optional options.
--- @return crap.Document
function crap.globals.unpublish(slug, opts) end

--- Validate global field data without persisting. Returns
--- `{ valid = true }` on success, or `{ valid = false, errors = {...} }`
--- on validation failure (errors keyed by field name). Globals always
--- validate in update mode against the singleton `default` row.
--- @param slug string  Global slug.
--- @param data_table table<string, any>  Field values to validate.
--- @param opts crap.GlobalValidateOptions?  Optional options.
--- @return crap.ValidateResult
function crap.globals.validate(slug, data_table, opts) end

-- ── crap.hooks ───────────────────────────────────────────────

--- Hook registration API.
--- @class crap.hooks
crap.hooks = {}

--- Register a hook function for an event. Fires for all collections.
--- @param event crap.HookEvent  The lifecycle event to hook into.
--- @param func fun(context: crap.HookContext): crap.HookContext  Hook function.
function crap.hooks.register(event, func) end

--- Remove a previously registered hook function (identity-based via rawequal).
--- @param event crap.HookEvent  The lifecycle event.
--- @param func fun(context: crap.HookContext): crap.HookContext  The function to remove.
function crap.hooks.remove(event, func) end

--- List all registered hook functions for an event.
--- @param event crap.HookEvent  The lifecycle event.
--- @return fun(context: crap.HookContext): crap.HookContext[] # Array of hook functions.
function crap.hooks.list(event) end

--- Events that trigger hooks.
--- @alias crap.HookEvent
--- | "before_validate"
--- | "before_change"
--- | "after_change"
--- | "before_read"
--- | "after_read"
--- | "before_delete"
--- | "after_delete"
--- | "before_broadcast"
--- | "before_render"


-- ── crap.richtext ────────────────────────────────────────────

--- Custom ProseMirror node registration and rendering.
--- @class crap.richtext
crap.richtext = {}

--- Register a custom `ProseMirror` node type.
--- @param name string  Node name (alphanumeric + underscores only).
--- @param spec crap.RichtextNodeSpec  Node specification.
function crap.richtext.register_node(name, spec) end

--- Spec for registering a custom richtext node. Parsed from the Lua
--- table the user passes to `crap.richtext.register_node(name, spec)`.
---
--- `attrs` is the only Lua-typed field that's converted to a Rust
--- representation eagerly (via `parse_fields`) so the call site can
--- run the type-allow-list validation against typed
--- `FieldDefinition`s rather than re-walking a Lua table. `render`
--- stays as `mlua::Function` because it's invoked from Rust later, and
--- `mlua::Function` is the natural type for that.
--- @class crap.RichtextNodeSpec
--- @field label? string Display label (defaults to `name`).
--- @field inline? boolean Whether the node is inline (default: `false` = block).
--- @field attrs? crap.FieldDefinition[] Attribute definitions (scalar types only: text, number, textarea, select, radio, checkbox, date, email, json, code). Use `crap.fields.*` factory functions.
--- @field searchable_attrs? string[] Attr names to include in FTS search index.
--- @field render? fun(attrs: table): string Server-side render function. Receives the node attrs as a Lua table; returns the rendered HTML string.

--- Render richtext content, replacing custom nodes with their rendered HTML.
--- Detects format automatically: starts with '{' = JSON, otherwise HTML.
--- @param content string  Richtext content (HTML or `ProseMirror` JSON).
--- @return string # Rendered HTML output.
function crap.richtext.render(content) end


-- ── crap.log ─────────────────────────────────────────────────

--- Structured logging (maps to Rust tracing).
--- @class crap.log
crap.log = {}

--- Log an info-level message.
--- @param msg string  Log message.
function crap.log.info(msg) end

--- Log a warning-level message.
--- @param msg string  Log message.
function crap.log.warn(msg) end

--- Log an error-level message.
--- @param msg string  Log message.
function crap.log.error(msg) end


-- ── crap.json ────────────────────────────────────────────────

--- JSON encode/decode.
--- @class crap.json
crap.json = {}

--- Encode a Lua value as a JSON string.
--- @param value any  Lua value to encode.
--- @return string # JSON string.
function crap.json.encode(value) end

--- Decode a JSON string into a Lua value.
--- @param str string  JSON string.
--- @return any # Decoded Lua value.
function crap.json.decode(str) end


-- ── crap.util ────────────────────────────────────────────────

--- Utility functions.
--- @class crap.util
crap.util = {}

--- Generate a URL-safe slug from a string.
--- @param str string  Input string.
--- @return string # Lowercased, hyphenated slug.
function crap.util.slugify(str) end

--- Generate a unique nanoid.
--- @return string # Random nanoid string.
function crap.util.nanoid() end

--- Current time as RFC 3339 string.
--- @return string # RFC 3339 timestamp.
function crap.util.date_now() end

--- Current Unix timestamp (seconds since epoch).
--- @return integer # Seconds since the Unix epoch.
function crap.util.date_timestamp() end

--- Parse a date string into a Unix timestamp. Supports RFC 3339,
--- "YYYY-MM-DD HH:MM:SS", and "YYYY-MM-DD".
--- @param s string  Date string (RFC 3339, `YYYY-MM-DD HH:MM:SS`, or `YYYY-MM-DD`).
--- @return integer # Seconds since the Unix epoch.
function crap.util.date_parse(s) end

--- Format a Unix timestamp with a chrono format string.
--- @param ts integer  Unix timestamp (seconds).
--- @param fmt string  Chrono format string (e.g. `"%Y-%m-%d %H:%M:%S"`).
--- @return string # Formatted date string.
function crap.util.date_format(ts, fmt) end

--- Add seconds to a Unix timestamp.
--- @param ts integer  Base timestamp.
--- @param secs integer  Seconds to add (may be negative).
--- @return integer # New timestamp (`ts + secs`).
function crap.util.date_add(ts, secs) end

--- Difference (in seconds) between two Unix timestamps (`a - b`).
--- @param a integer  First timestamp.
--- @param b integer  Second timestamp.
--- @return integer # Seconds elapsed (`a - b`).
function crap.util.date_diff(a, b) end


-- ── crap.util — table helpers ────────────────────────────

--- Deep merge two tables. b overwrites a. Returns a new table.
--- @param a table  Base table.
--- @param b table  Override table.
--- @return table merged
function crap.util.deep_merge(a, b) end

--- Return a table with only the listed keys.
--- @param tbl table        Source table.
--- @param keys string[]    Keys to keep.
--- @return table
function crap.util.pick(tbl, keys) end

--- Return a table without the listed keys.
--- @param tbl table        Source table.
--- @param keys string[]    Keys to remove.
--- @return table
function crap.util.omit(tbl, keys) end

--- Extract all keys from a table as an array.
--- @param tbl table
--- @return string[]
function crap.util.keys(tbl) end

--- Extract all values from a table as an array.
--- @param tbl table
--- @return any[]
function crap.util.values(tbl) end

--- Map a function over an array table.
--- @param tbl any[]              Array to map over.
--- @param fn  fun(v: any, i: integer): any  Mapping function.
--- @return any[]
function crap.util.map(tbl, fn) end

--- Filter an array table by a predicate.
--- @param tbl any[]              Array to filter.
--- @param fn  fun(v: any, i: integer): boolean  Predicate function.
--- @return any[]
function crap.util.filter(tbl, fn) end

--- Find the first element matching a predicate.
--- @param tbl any[]              Array to search.
--- @param fn  fun(v: any, i: integer): boolean  Predicate function.
--- @return any?
function crap.util.find(tbl, fn) end

--- Check if an array contains a value.
--- @param tbl any[]  Array to search.
--- @param value any  Value to find.
--- @return boolean
function crap.util.includes(tbl, value) end

--- Check if a table has no entries.
--- @param tbl table
--- @return boolean
function crap.util.is_empty(tbl) end

--- Shallow copy a table.
--- @param tbl table
--- @return table
function crap.util.clone(tbl) end

-- ── crap.util — string helpers ───────────────────────────

--- Strip leading and trailing whitespace.
--- @param str string
--- @return string
function crap.util.trim(str) end

--- Split a string by separator.
--- @param str string  Input string.
--- @param sep string  Separator string.
--- @return string[]
function crap.util.split(str, sep) end

--- Check if a string starts with a prefix.
--- @param str string
--- @param prefix string
--- @return boolean
function crap.util.starts_with(str, prefix) end

--- Check if a string ends with a suffix.
--- @param str string
--- @param suffix string
--- @return boolean
function crap.util.ends_with(str, suffix) end

--- Truncate a string to a max length with optional suffix.
--- @param str string       Input string.
--- @param max_len integer  Maximum length.
--- @param suffix? string   Suffix to append when truncated (default: "...").
--- @return string
function crap.util.truncate(str, max_len, suffix) end

-- ── crap.auth ────────────────────────────────────────────────

--- Password hashing and verification helpers.
--- @class crap.auth
crap.auth = {}

--- Hash a plaintext password (Argon2id).
--- @param password string  Plaintext password.
--- @return string # Hashed password.
function crap.auth.hash_password(password) end

--- Verify a password against a hash.
--- @param password string  Plaintext password.
--- @param hash string  Stored hash.
--- @return boolean
function crap.auth.verify_password(password, hash) end

--- Return the currently authenticated user document for the in-flight request, or nil.
--- Returns nil from init.lua, on unauthenticated requests, or outside a hook context.
--- @return crap.Document?
function crap.auth.user() end

--- The standard 3-method auth set: `password_login` + `bearer` (all
--- surfaces) + `session_cookie` (admin only). Use in collection
--- definitions:
---
--- ```lua
--- auth = {
---   enabled = true,
---   methods = crap.auth.default_methods(),
--- }
--- ```
--- @return crap.AuthMethod[]
function crap.auth.default_methods() end

--- Returns `default_methods()` with `extras` appended. The most
--- common shape for "I want the standard auth plus my own strategy":
---
--- ```lua
--- auth = {
---   enabled = true,
---   methods = crap.auth.with_defaults({
---     { type = "strategy", name = "api-key",
---       authenticate = "hooks.auth.api_key",
---       activates_on = { header = "x-api-key" },
---       surfaces = { "grpc" } },
---   }),
--- }
--- ```
--- @param extras crap.AuthMethod[]?  Methods to append after the defaults.
--- @return crap.AuthMethod[]
function crap.auth.with_defaults(extras) end


-- ── crap.access ──────────────────────────────────────────────

--- Programmatic access-control checks for collections and globals,
--- mirroring the same evaluator the CMS uses internally for read /
--- create / update / delete decisions.
--- @class crap.access
crap.access = {}

--- Evaluate the access function for a collection/global + operation against the
--- current user. Returns `"allowed"`, `"denied"`, or a constraint-filter table.
--- @param collection string  Collection or global slug.
--- @param operation string  Operation: `"read"`, `"create"`, `"update"`, `"delete"`, or `"trash"`.
--- @return "allowed" | "denied" | table # "allowed" / "denied" string, or a `{ field = filter, ... }` constraint table.
function crap.access.check(collection, operation) end

--- Return the names of fields the current user cannot read for this
--- collection/global. Pass `document` for a data-aware check (matches the
--- enforcement strip for that row — use it for an edit form); omit it for a
--- categorical, role-only check (`ctx.data` / `ctx.document` nil — use it for a
--- create form). For UI gating only — enforcement is the per-document strip.
--- @param collection string  Collection or global slug.
--- @param document table?  Optional document to evaluate data-dependent rules against (sets `ctx.data` / `ctx.document`). Omit for a categorical, role-only check.
--- @return string[] # Names of fields the current user cannot read.
function crap.access.field_read_denied(collection, document) end

--- Return the names of fields the current user cannot write for this
--- collection/global under the given operation (`"create"` or `"update"`). Pass
--- `document` for a data-aware check (matches the enforcement strip — use it for
--- an edit form, where the row is known); omit it for a categorical, role-only
--- check (use it for a create form). For UI gating only — enforcement is the
--- per-document strip.
--- @param collection string  Collection or global slug.
--- @param operation string  Operation: `"create"` or `"update"`.
--- @param document table?  Optional document to evaluate data-dependent rules against (sets `ctx.data` / `ctx.document`). Omit for a categorical, role-only check.
--- @return string[] # Names of fields the current user cannot write.
function crap.access.field_write_denied(collection, operation, document) end


-- ── crap.env ─────────────────────────────────────────────────

--- Read-only access to environment variables.
--- @class crap.env
crap.env = {}

--- Get an environment variable.
--- @param key string  Variable name.
--- @return string? # Value or nil if not set.
function crap.env.get(key) end


-- ── crap.http ────────────────────────────────────────────────

--- Outbound HTTP client (blocking, runs inside spawn_blocking context).
--- @class crap.http
crap.http = {}

--- Make an outbound HTTP request. Blocking — safe inside `spawn_blocking`
--- contexts (which is where Lua hooks run). DNS-pinned when private
--- networks are disabled in `crap.toml`.
--- @param opts crap.HttpRequest  Request options.
--- @return crap.HttpResponse
function crap.http.request(opts) end

--- Options table for `crap.http.request`. Unknown keys are rejected.
--- @class crap.HttpRequest
--- @field url string Request URL.
--- @field method? string HTTP method (default: `"GET"`).
--- @field headers? table<string, string> Request headers.
--- @field body? string Request body.
--- @field timeout? number Request timeout in seconds; fractional values allowed (e.g. `0.5` = 500 ms). Default: `30`.

--- Response returned by `crap.http.request(opts)`. Both `LuaAnnotation`
--- (for `types/crap.lua`) and `Serialize` (for the runtime
--- `lua.to_value(&self)` conversion); the same Rust struct is the
--- single source of truth.
--- @class crap.HttpResponse
--- @field status integer HTTP status code.
--- @field headers table<string, string> Response headers.
--- @field body string Response body.


-- ── crap.email ───────────────────────────────────────────────

--- Email sending (requires SMTP configuration in crap.toml).
--- @class crap.email
crap.email = {}

--- Send an email via SMTP. Blocking — safe to call from hooks.
--- Returns true on success. If email is not configured (`smtp_host` empty), logs a warning and returns true (no-op).
--- @param opts crap.EmailOptions  Email options.
--- @return boolean # True on success (also true when SMTP is disabled — call is a no-op).
function crap.email.send(opts) end

--- Queue an email for async delivery via the job system. Returns the job
--- run ID. Per-call `retries` override on `opts` takes precedence over
--- the queue default from `[jobs.queues.email] retries`.
--- @param opts crap.EmailOptions  Email options (with optional `retries` override).
--- @return string # Queued job ID.
function crap.email.queue(opts) end

--- Register a custom email provider's handler. **Init-only** — call from
--- `init.lua` when `[email] provider = "custom"`. Stores `handler.send`
--- as `crap._email_send`; the custom provider delegates every send to it.
--- @param handler { send: fun(opts: crap.EmailOptions) }  Provider handler. Must have a `send(opts)` function.
function crap.email.register(handler) end

--- Options table for `crap.email.send` / `crap.email.queue`.
--- Unknown keys are rejected.
--- @class crap.EmailOptions
--- @field to string Recipient email address.
--- @field subject string Email subject line.
--- @field html string HTML email body.
--- @field text? string Plain-text fallback body.
--- @field retries? integer Per-call override of the queue retry count (only honoured by `crap.email.queue`).


-- ── crap.storage ─────────────────────────────────────────────

--- Register a custom upload-storage backend (for `[upload] storage = "custom"`).
--- @class crap.storage
crap.storage = {}

--- Register a custom storage backend's handler. **Init-only** — call from
--- `init.lua` when `[upload] storage = "custom"`. Stores the handler as
--- `crap._storage`; the custom backend delegates every operation to it.
--- @param handler { put: fun(key: string, data: string, content_type: string), get: fun(key: string): string?, delete: fun(key: string), exists?: fun(key: string): boolean }  Storage handler. `put`/`get`/`delete` required; `exists` optional. `get` returns nil for a missing key.
function crap.storage.register(handler) end


-- ── crap.config ──────────────────────────────────────────────

--- Read-only access to crap.toml configuration values.
--- Values are a snapshot from startup — changes to crap.toml after
--- startup won't be reflected until restart.
--- @class crap.config
crap.config = {}

--- Get a configuration value using dot notation.
--- @param key string  Dot-separated config key (e.g., "server.admin_port").
--- @return any # Value at the key path, or nil if any segment is missing or not a table.
function crap.config.get(key) end


-- ── crap.locale ──────────────────────────────────────────────

--- Locale configuration access (read-only).
--- Available in init.lua and hook functions.
--- @class crap.locale
crap.locale = {}

--- Get the default locale (e.g., `"en"`).
--- @return string # Default locale code.
function crap.locale.get_default() end

--- List every configured locale (in the order declared in `crap.toml`).
--- @return string[] # Configured locale codes (insertion order).
function crap.locale.get_all() end

--- Whether localization is enabled (true when at least one locale is
--- configured).
--- @return boolean # True iff at least one locale is configured.
function crap.locale.is_enabled() end


-- ── crap.crypto ──────────────────────────────────────────────

--- Cryptographic helpers. Keys derived from the auth secret in crap.toml.
--- @class crap.crypto
crap.crypto = {}

--- SHA-256 hash of a string, returned as hex.
--- @param data string  Data to hash.
--- @return string # 64-character hex string.
function crap.crypto.sha256(data) end

--- HMAC-SHA256 of data with a key, returned as hex.
--- Always compare HMACs with `crap.crypto.constant_time_eq(...)`, never with
--- `==`, to avoid timing attacks.
--- @param data string  Data to authenticate.
--- @param key string  HMAC key.
--- @return string # 64-character hex string.
function crap.crypto.hmac_sha256(data, key) end

--- Constant-time byte-string equality. Use this — never `==` — to compare
--- HMAC tags, tokens, or any secret value. Length mismatch and content
--- mismatch are indistinguishable from the return value.
--- @param a string  First value.
--- @param b string  Second value.
--- @return boolean # True iff `a` equals `b`.
function crap.crypto.constant_time_eq(a, b) end

--- Base64-encode a string.
--- @param str string  String to encode.
--- @return string
function crap.crypto.base64_encode(str) end

--- Base64-decode a string.
--- @param str string  Base64 string to decode.
--- @return string
function crap.crypto.base64_decode(str) end

--- Generate `n` random bytes, returned as hex.
--- @param n integer  Number of random bytes.
--- @return string # Hex string of length 2*n.
function crap.crypto.random_bytes(n) end

--- Encrypt plaintext using AES-256-GCM. Key derived from auth secret.
--- Returns base64-encoded ciphertext (nonce prepended).
--- @param plaintext string
--- @return string # Base64-encoded ciphertext.
function crap.crypto.encrypt(plaintext) end

--- Decrypt ciphertext produced by `encrypt`.
--- @param ciphertext string  Base64-encoded ciphertext from `encrypt`.
--- @return string # Decoded plaintext string.
function crap.crypto.decrypt(ciphertext) end


-- ── crap.schema ──────────────────────────────────────────────

--- Schema introspection API (read-only). Reads from the loaded registry.
--- @class crap.schema
crap.schema = {}

--- Get a collection's schema definition.
--- @param slug string  Collection slug.
--- @return crap.SchemaCollection?
function crap.schema.get_collection(slug) end

--- Get a global's schema definition.
--- @param slug string  Global slug.
--- @return crap.SchemaCollection?
function crap.schema.get_global(slug) end

--- List all collection slugs and labels.
--- @return { slug: string, labels: { singular?: string, plural?: string } }[]
function crap.schema.list_collections() end

--- List all global slugs and labels.
--- @return { slug: string, labels: { singular?: string, plural?: string } }[]
function crap.schema.list_globals() end

--- Result of `crap.schema.get_collection` / `crap.schema.get_global`.
--- Globals reuse the same shape with `has_*` flags set to false.
--- @class crap.SchemaCollection
--- @field slug string Collection or global slug.
--- @field labels { singular?: string, plural?: string } Localized labels (singular/plural, resolved to the default locale).
--- @field timestamps boolean Whether the collection auto-manages `created_at` / `updated_at`.
--- @field has_auth boolean Whether the collection is an auth collection.
--- @field has_upload boolean Whether the collection is an upload collection.
--- @field has_versions boolean Whether the collection has versioning enabled.
--- @field has_drafts boolean Whether the collection has draft support.
--- @field fields crap.SchemaField[] Field definitions, in declaration order.

--- One field in a `SchemaCollection`. The same shape covers all field
--- types — keys not relevant to a given type are simply omitted.
---
--- Nesting mirrors the *write* shape on `crap.FieldDefinition` so the
--- schema-read surface reads back the way the user wrote it:
--- admin-UI hints live under `admin`, join config lives under `join`,
--- relationship config under `relationship`.
--- @class crap.SchemaField
--- @field name string
--- @field type string
--- @field required boolean
--- @field localized boolean
--- @field unique boolean
--- @field relationship? { collection: string | string[], has_many: boolean, max_depth?: integer }
--- @field min_length? integer
--- @field max_length? integer
--- @field min? number
--- @field max? number
--- @field integer? boolean
--- @field has_many? boolean
--- @field min_date? string
--- @field max_date? string
--- @field picker_appearance? crap.PickerAppearance `Date`-field input type. Matches the user's `crap.FieldDefinition.picker_appearance` value (`"dayOnly"`, `"dayAndTime"`, …); absent for non-date fields.
--- @field admin? { language?: string, features?: string[], picker?: string } Admin-UI hints: `language` / `features` / `picker`. Nested to mirror the write-side `crap.FieldAdmin` shape — the user writes `admin = { picker = "card" }` and reads back `field.admin.picker`.
--- @field join? { collection: string, on: string } `Join` field config. Absent for every non-`Join` field; for `Join` fields, mirrors the user's `crap.FieldDefinition.join` value (`{ collection, on }`).
--- @field options? { label: string, value: string }[]
--- @field fields? crap.SchemaField[] Sub-fields for `Group` / `Array` field types (recursive).
--- @field blocks? { type: string, label?: string, group?: string, image_url?: string, fields: crap.SchemaField[] }[] Block definitions for `Blocks` field types.
--- @field tabs? { label: string, description?: string, fields: crap.SchemaField[] }[] Tab definitions for `Tabs` layout field types. Each tab carries its own sub-fields (Tabs store children under `.tabs`, not `.fields`).


-- ── crap.jobs ────────────────────────────────────────────────

--- Background job definition and queuing API.
--- @class crap.jobs
crap.jobs = {}

--- Define a background job. Call in init.lua or jobs/*.lua files.
--- The handler function receives a context table with `data` and `job` fields,
--- and has full CRUD access (crap.collections.find/create/update/delete).
--- @param slug string  Unique job identifier.
--- @param config crap.JobDefinitionConfig  Job configuration.
function crap.jobs.define(slug, config) end

--- Display labels for a job definition.
--- @class crap.JobLabels
--- @field singular? string Display label for the job (e.g., "Cleanup Expired Posts").

--- Typed `config` table passed to `crap.jobs.define(slug, config)`.
--- @class crap.JobDefinitionConfig
--- @field handler? string | crap.HookRef Lua function ref for the job handler (required, e.g., `"jobs.cleanup.run"`). May carry per-definition options exposed to the handler as `ctx.options`.
--- @field schedule? string Cron expression (e.g., `"0 3 * * *"`). When set, the job runs on this schedule. Accepts both 5-field and 6/7-field forms.
--- @field queue? string Queue name (default: `"default"`).
--- @field retries? integer Max retry attempts on failure (default: `0`).
--- @field timeout? integer Seconds before a running job is marked failed (default: `60`).
--- @field concurrency? integer Max concurrent runs of this job (default: `1`).
--- @field priority? integer Default scheduling priority for this job. Used when a queue site doesn't pass an explicit `{ priority = N }`. Higher = claimed sooner; negative = run only when otherwise idle. Default: `0`.
--- @field skip_if_running? boolean Skip scheduled run if a previous run is still active (default: `true`).
--- @field labels? crap.JobLabels Display labels for the admin UI.
--- @field access? string | crap.HookRef Lua function ref for access control on gRPC/CLI trigger.

--- Job handler context. Passed to the handler function configured on a
--- `crap.jobs.define` definition.
--- @class crap.JobHandlerContext
--- @field data table<string, any> Input data from `crap.jobs.queue(...)` (or `{}` for cron-only runs).
--- @field job crap.JobInfo Job metadata for the current run.
--- @field options? table Per-definition options from the handler's `{ ref, options }` table; `nil` when the handler was configured as a bare ref string.

--- Job metadata for the current run.
--- @class crap.JobInfo
--- @field id string Nanoid id of this job run (e.g. to record it or correlate logs).
--- @field slug string Job definition slug.
--- @field queue string Queue this run is executing on.
--- @field attempt integer Current attempt number (1-based).
--- @field max_attempts integer Total max attempts.
--- @field priority integer Scheduling priority this run was queued with (higher = claimed sooner).
--- @field unique_key? string Dedup key, when the run was queued with `{ unique = "..." }`. `nil` for normal enqueues.
--- @field scheduled_by? string How this run was triggered: `"cron"`, `"hook"`, `"grpc"`, or `"cli"`. `nil` if unknown.
--- @field queued_at? string ISO-8601 timestamp when the run was queued. `nil` if unknown.

--- Queue a job for background execution. Returns the job run ID.
--- Only available inside hooks with transaction context.
--- @param slug string  Job slug (must be previously defined).
--- @param data table?  Input data passed to the handler (default: {}).
--- @param opts table?  Options table. Supports `priority` (integer, default falls back to the job definition's priority; higher = sooner), `delay` (integer seconds or duration string `"5m"` / `"30s"` / `"1h"`; default 0 = immediate), and `unique` (string dedup key — if another pending/running job has the same slug+unique key, returns its id instead of inserting a duplicate).
--- @return string # The queued job run ID.
function crap.jobs.queue(slug, data, opts) end

-- ── crap.pages ───────────────────────────────────────────────

--- Declare custom admin pages and their sidebar metadata. The page
--- TEMPLATE lives at `<config_dir>/templates/pages/<slug>.hbs`; this
--- API only adds the sidebar entry and the optional access gate.
--- @class crap.pages
crap.pages = {}

--- Register a custom admin page. Must be called from init.lua or a
--- definition file — runtime registration is rejected (custom pages are
--- read once at startup, so a runtime call would silently land in the
--- current VM's named registry without ever appearing on the sidebar
--- or routes).
--- @param slug string  Page slug (a-z, 0-9, '-', '_'); also the route under `/admin/p/<slug>`.
--- @param opts crap.PageOptions  Sidebar / access metadata (all keys optional).
function crap.pages.register(slug, opts) end

--- List every registered custom page as a table
--- `{ slug, section?, label?, icon?, access = "public"|"gated" }` (in iteration
--- order — not deterministic across runs). Mirrors `crap.routes.list()`.
--- @return crap.PageInfo[]
function crap.pages.list() end

--- Sidebar / access metadata for a custom admin page.
--- @class crap.PageOptions
--- @field section? string Sidebar section heading (e.g., `"Tools"`).
--- @field label? string Sidebar label (defaults to title-cased slug when omitted).
--- @field icon? string Material Symbols icon name.
--- @field access? string | crap.HookRef Lua function ref for access control (resolved against the same registry as collection-level `access.*`). A bare ref string or a `{ ref, options }` table whose options reach the gate as `ctx.options`.


--- @class crap.PageInfo
--- @field slug string The page slug (URL segment + template filename).
--- @field section? string Sidebar section heading.
--- @field label? string Sidebar label.
--- @field icon? string Material Symbols icon name.
--- @field access "public" | "gated"

-- ── crap.routes ──────────────────────────────────────────────

--- Declare custom HTTP endpoints. The handler lives at
--- `<config_dir>/routes/<name>.lua` and returns `function(ctx) ... end`;
--- this API mounts it on the admin server with its access/rate-limit metadata.
--- @class crap.routes
crap.routes = {}

--- Register a custom HTTP route. Init-phase only — runtime registration has no
--- effect (routes are mounted once at startup).
--- @param def table  Route definition (path, method, handler, …).
function crap.routes.register(def) end

--- List every registered route (in declaration order) as a table
--- `{ path = string, methods = string[], access = "public"|"disabled"|"gated" }`.
--- @return crap.RouteInfo[]
function crap.routes.list() end

--- The `ctx` table a custom route handler receives. The dispatcher pre-parses
--- cookies, query, and (by content-type) the body, so the handler reads them
--- directly instead of plumbing the raw request itself.
--- @class crap.RouteContext
--- @field method string HTTP method, upper-cased (`"GET"`, `"POST"`, …).
--- @field path string The matched route path.
--- @field params table<string, string> Path parameters (`/items/{id}` → `ctx.params.id`).
--- @field query table<string, string> Parsed query string parameters.
--- @field headers table<string, string> Request headers (lowercase keys).
--- @field cookies table<string, string> Parsed request cookies (`ctx.cookies.crap_session`).
--- @field body? string Raw request body as a string, when present.
--- @field json? table Parsed body when `Content-Type` is JSON; `nil` otherwise.
--- @field form? table<string, string> Parsed body for `application/x-www-form-urlencoded`; `nil` otherwise.
--- @field user? crap.AuthUser The authenticated user document, when the request carried a valid session/bearer; `nil` for anonymous requests.
--- @field collection? string The auth collection the user belongs to; `nil` when anonymous.
--- @field ip string The client's remote IP (spoof-resistant; honors trusted proxies).
--- @field ui_locale? string Admin UI locale, when the request originated from the admin UI.
--- @field options? table Per-route options from `crap.routes.register({ options = … })`.


--- A custom-route handler: receives the request context and returns a
--- response envelope `{ status?, headers?, cookies?, body?, json?, redirect? }`,
--- a bare string (200 text), or nil (404).
--- @alias crap.route_handler_fn fun(ctx: crap.RouteContext): table | string | nil

--- @class crap.RouteInfo
--- @field path string The mounted path.
--- @field methods string[] HTTP methods this route answers.
--- @field access "public" | "disabled" | "gated"

-- ── crap.template_data ───────────────────────────────────────

--- Register named template-data functions called lazily by the
--- `{{data "name"}}` Handlebars helper. Each function is invoked
--- once per HTTP request when first referenced; results are not
--- cached across requests.
--- @class crap.template_data
crap.template_data = {}

--- Register a named template-data function. Must be called from
--- init.lua or a definition file — runtime registration only lands in
--- one VM of the pool and is intermittent across requests.
--- @param name string  Unique name (used as `{{data "name"}}` in templates).
--- @param func function  Lua function called on each `{{data}}` lookup; returns any JSON-encodable value.
function crap.template_data.register(name, func) end

--- List the names of every registered template-data function (in
--- iteration order — not deterministic across runs).
--- @return string[]
function crap.template_data.list() end


-- ── crap.transaction — explicit multi-step atomicity ───────────────

--- Wrap `fn` in a single IMMEDIATE transaction. Use inside job
--- handlers when multiple CRUD operations need to be atomic — by
--- default each Lua CRUD call in a job opens its own short-lived
--- transaction (pool-mode), so a `find` followed by an `update` are
--- two separate writes. Wrap them in `crap.transaction(function()
--- ... end)` to make the block atomic.
---
--- Returns whatever `fn` returns. Errors raised inside `fn` roll back
--- the transaction and propagate as Lua errors. Inside a hook (which
--- already runs in the parent's write transaction) this is a
--- pass-through.
---
--- Only valid from a job handler — calling from init.lua / collection
--- definitions / top-level scripts raises a runtime error.
---
--- @generic T
--- @param fn fun(): T
--- @return T
function crap.transaction(fn) end


-- ── crap.tx — transaction-outcome effects ──────────────────────────

--- @class crap.tx
crap.tx = {}

--- Defer a side effect until the surrounding write transaction has
--- **committed**. `ref` is a hook reference (`"hooks.module.fn"`);
--- `data` is captured immediately (plain, JSON-serializable values
--- only) and passed to the handler as `ctx.data` together with
--- `ctx.outcome = "commit"`. The handler runs post-commit in
--- pool-mode: CRUD works, each call in its own short transaction.
--- Handler errors are logged, never propagated (the commit already
--- happened). Registering with an unresolvable ref or an
--- unserializable payload raises immediately, rolling the
--- transaction back.
---
--- Only valid inside a write lifecycle hook or `crap.transaction(fn)`.
---
--- @param ref string
--- @param data? table
function crap.tx.on_commit(ref, data) end

--- Register a compensation that runs only if the surrounding write
--- transaction **rolls back** (hook error, validation failure, or
--- failed commit). Same contract as `crap.tx.on_commit`, with
--- `ctx.outcome = "rollback"`.
---
--- @param ref string
--- @param data? table
function crap.tx.on_rollback(ref, data) end


-- ── crap.uploads ─────────────────────────────────────────────

--- Upload helpers: signed serve-URL minting for cross-origin private media.
--- @class crap.uploads
crap.uploads = {}

--- Mint a time-boxed signed URL for a stored upload proxy path, so a browser
--- on another origin (or behind a CDN) can fetch a **private** file without a
--- session cookie or Bearer token. The returned URL is a capability: whoever
--- holds it can fetch the file until it expires — authorize first (typically
--- you sign values from a document that already passed the read pipeline,
--- e.g. inside an `after_read` hook) and keep TTLs short (capped at 30 days).
---
--- **Never sign a client-supplied path.** This function is a signing oracle:
--- whatever it signs becomes fetchable by anyone holding the URL. Passing
--- `ctx.query`/`ctx.body` values from a custom route hands out capabilities
--- for arbitrary private uploads.
--- @param url string  A stored upload URL — the `/uploads/…` proxy path from `doc.url` or a `{size}_url` column.
--- @param expires_in integer?  Seconds until expiry (default 300). Must be positive.
--- @return string # The path with `exp` and `sig` query parameters appended.
function crap.uploads.sign_url(url, expires_in) end


