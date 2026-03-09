---@meta crap
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


-- ── Field Types ──────────────────────────────────────────────

--- Supported field types for collection and global definitions.
--- @alias crap.FieldType
--- | "text"         # Single-line string
--- | "number"       # Integer or float
--- | "textarea"     # Multi-line text
--- | "richtext"     # Rich text (stored as HTML by default, or JSON with admin.format = "json")
--- | "select"       # Single or multi select from options (has_many for multi)
--- | "radio"        # Radio button group (same as select, renders as radio buttons)
--- | "checkbox"     # Boolean (true/false)
--- | "date"         # ISO 8601 date/datetime
--- | "email"        # Validated email address
--- | "json"         # Arbitrary JSON blob
--- | "upload"       # File upload (references media collection; has_many for multi-file)
--- | "relationship" # Reference to another collection
--- | "array"        # Repeatable sub-fields
--- | "group"        # Visual grouping (no extra table)
--- | "blocks"       # Flexible content blocks
--- | "row"          # Layout-only horizontal grouping (no prefix)
--- | "collapsible"  # Layout-only collapsible section (no prefix)
--- | "tabs"         # Layout-only tabbed container (no prefix)
--- | "code"         # Code editor (CodeMirror, admin.language for mode)
--- | "join"         # Virtual reverse relationship (read-only, no column)

--- A string that can be plain or per-locale.
--- Plain: `"Title"` — used as-is.
--- Localized: `{ en = "Title", de = "Titel" }` — resolved based on admin locale.
--- @alias crap.LocalizedString string | table<string, string>

--- @alias crap.FieldWidth "full" | "half" | "third"

--- @class crap.RelationshipConfig
--- @field collection string|string[]  Target collection slug, or an array of slugs for polymorphic relationships (required). Example: `"posts"` or `{ "posts", "pages" }`.
--- @field has_many?  boolean          Many-to-many relationship via junction table (default: false).
--- @field max_depth? integer          Per-field max population depth. Limits depth regardless of request-level depth.

--- @class crap.SelectOption
--- @field label crap.LocalizedString Display text in the admin UI.
--- @field value string Stored value.

--- @class crap.FieldAdmin
--- @field label?       crap.LocalizedString   UI label (defaults to field name).
--- @field description? crap.LocalizedString   Help text shown below the input.
--- @field placeholder? crap.LocalizedString   Input placeholder text.
--- @field hidden?      boolean  Hide from admin UI (default: false).
--- @field readonly?    boolean  Non-editable in admin (default: false).
--- @field width?       crap.FieldWidth  Field width: "full", "half", or "third".
--- @field collapsed?   boolean  Start collapsed in admin UI — groups, collapsibles, array/block rows (default: true). Set `false` to start expanded.
--- @field position?    string   "main" or "sidebar".
--- @field condition?   string   Lua function ref for conditional show/hide.
--- @field components?  table    Override admin UI partials for this field.
--- @field label_field? string   Sub-field name to use as row label in admin (arrays/blocks). The value of this sub-field is shown as the row title. For blocks, per-block `label_field` on `BlockDefinition` takes priority.
--- @field row_label?   string   Lua function ref for computed row labels (arrays/blocks). Receives the row data table, returns a display string or nil. Takes priority over `label_field`. Signature: `fun(row: table): string?`.
--- @field labels?      crap.FieldAdminLabels  Custom singular/plural labels for row items (e.g., `{ singular = "Slide", plural = "Slides" }` → "Add Slide" button).
--- @field step?        string   Step value for number inputs (default: "any"). Use "1" for integers, "0.01" for cents, etc.
--- @field rows?        integer  Number of rows for textarea fields (default: 8).
--- @field language?    string   Language mode for code fields (default: "json"). Options: "json", "javascript", "html", "css", "python", "plain".
--- @field features?    string[] Enabled toolbar features for richtext fields. When absent, all features are enabled. Options: "bold", "italic", "code", "link", "heading", "blockquote", "orderedList", "bulletList", "codeBlock", "horizontalRule".
--- @field format?      string   Storage format for richtext fields: "html" (default) or "json" (ProseMirror JSON). FTS extracts plain text from JSON automatically.
--- @field nodes?       string[] Custom ProseMirror node types for richtext fields. Names must match nodes registered via `crap.richtext.register_node()`.
--- @field picker?      string   Picker UI style. For blocks fields: "select" (default) uses a dropdown, "card" uses a visual card grid. For upload fields: "drawer" (default) adds a browse button with thumbnail grid, "none" disables it. For relationship fields: "drawer" adds a browse button with searchable list.

--- Custom validation function type.
--- Return nil or true if valid, return a string error message if invalid.
--- @alias crap.ValidateFunction fun(value: any, context: crap.ValidateContext): string?

--- @class crap.ValidateContext
--- @field collection string              Collection slug.
--- @field field_name string              Name of the field being validated.
--- @field data       table<string, any>  Full document data.

--- @class crap.FieldHooks
--- @field before_validate? string[] Hook refs to run before field validation (value normalizers).
--- @field before_change?   string[] Hook refs to run after validation, before write.
--- @field after_change?    string[] Hook refs to run after create/update write.
--- @field after_read?      string[] Hook refs to run after read, before response.

--- Field hook function type.
--- Receives the field value and a context table, returns the (possibly modified) value.
--- @alias crap.FieldHookFn fun(value: any, context: crap.FieldHookContext): any

--- @class crap.FieldHookContext
--- @field field_name  string              Name of the field being processed.
--- @field collection  string              Collection slug.
--- @field operation   string              The operation: "create", "update", "find", "find_by_id", etc.
--- @field data        table<string, any>  Full document data (read-only snapshot).
--- @field user?       table               Authenticated user document (nil if unauthenticated or no auth collection).
--- @field ui_locale?  string              Admin UI locale code (e.g., "en", "de"). Nil if not set.
---
--- Note: before_validate, before_change, and after_change field hooks have full
--- CRUD access (crap.collections.find/create/update/delete) via the parent transaction.
--- after_read field hooks do NOT have CRUD access.

--- @class crap.FieldTab
--- @field label       string                    Tab label displayed in the tab bar (required).
--- @field description? string                   Help text shown inside the tab panel.
--- @field fields      crap.FieldDefinition[]    Fields within this tab.

--- @class crap.BlockDefinition
--- @field type         string                    Block type identifier (required).
--- @field label?       crap.LocalizedString      Display label for the block type (defaults to type name).
--- @field label_field? string                    Sub-field name to use as row label for this block type. Overrides the field-level `admin.label_field` for blocks of this type.
--- @field group?       string                    Group name for organizing blocks in the picker dropdown (rendered as `<optgroup>`).
--- @field image_url?   string                    Image URL for displaying an icon/thumbnail in the block picker. When any block has an image, the picker renders as a visual card grid.
--- @field fields       crap.FieldDefinition[]    Fields within this block type.

--- @alias crap.PickerAppearance "dayOnly" | "dayAndTime" | "timeOnly" | "monthOnly"

--- @class crap.McpFieldConfig
--- @field description? string  Description shown in MCP tool JSON Schema for this field.

--- @class crap.McpCollectionConfig
--- @field description? string  Description used as the MCP tool description for this collection/global.

--- @class crap.FieldDefinition
--- @field name          string            Column name (required).
--- @field type          crap.FieldType    Field type (required).
--- @field required?     boolean           Validation: must have a value (default: false).
--- @field unique?       boolean           Unique constraint (default: false).
--- @field index?        boolean           Create a B-tree index on this column (default: false). Skipped when unique=true (already indexed).
--- @field localized?    boolean           Per-locale values (default: false).
--- @field default_value? any              Default value on create.
--- @field hidden?       boolean           Hide from API responses (default: false).
--- @field validate?     string            Lua function ref (module.function format) called as crap.ValidateFunction.
--- @field hooks?        crap.FieldHooks   Per-field lifecycle hooks (value transformers, no CRUD access).
--- @field options?      crap.SelectOption[] Options for "select" field type.
--- @field relationship? crap.RelationshipConfig  Relationship config (preferred syntax).
--- @field relation_to?  string            Target collection (legacy flat syntax).
--- @field has_many?     boolean           Multi-value: for select/text = JSON array in TEXT column, for number = JSON array of number strings, for relationship/upload = junction table (default: false). Text/number render as tag inputs in admin.
--- @field fields?       crap.FieldDefinition[] Sub-fields for "array", "group", "row", and "collapsible" types.
--- @field blocks?       crap.BlockDefinition[] Block type definitions for "blocks" type.
--- @field tabs?         crap.FieldTab[]        Tab definitions for "tabs" type. Each tab has a label and fields.
--- @field admin?        crap.FieldAdmin   Admin UI display options.
--- @field access?       crap.FieldAccess  Field-level access control (read/create/update).
--- @field mcp?          crap.McpFieldConfig  MCP tool schema options.
--- @field picker_appearance? crap.PickerAppearance  For "date" fields: controls HTML input type and storage format. "dayOnly" (default) = date picker, stored as `YYYY-MM-DDT12:00:00.000Z`. "dayAndTime" = datetime-local picker, stored as full ISO 8601 UTC. "timeOnly" = time picker, stored as `HH:MM`. "monthOnly" = month picker, stored as `YYYY-MM`.
--- @field min_rows?    integer  Minimum number of rows for array/blocks fields. Validated on create/update (skipped for drafts).
--- @field max_rows?    integer  Maximum number of rows for array/blocks fields. Validated on create/update (skipped for drafts). Admin UI disables "Add" button at max.
--- @field min_length?  integer  Minimum string length for text/textarea fields. Validated server-side + HTML minlength attr.
--- @field max_length?  integer  Maximum string length for text/textarea fields. Validated server-side + HTML maxlength attr.
--- @field min?         number   Minimum numeric value for number fields. Validated server-side + HTML min attr.
--- @field max?         number   Maximum numeric value for number fields. Validated server-side + HTML max attr.
--- @field min_date?    string   Minimum date (ISO format "YYYY-MM-DD") for date fields. Validated server-side + HTML min attr.
--- @field max_date?    string   Maximum date (ISO format "YYYY-MM-DD") for date fields. Validated server-side + HTML max attr.
--- @field collection?  string   For "join" type: target collection slug whose documents reference this one.
--- @field on?          string   For "join" type: field name on the target collection that holds the reference to this document.

--- @class crap.FieldAdminLabels
--- @field singular? crap.LocalizedString  Custom singular label for row items (e.g., "Slide" → "Add Slide" button).
--- @field plural?   crap.LocalizedString  Custom plural label for the field header.


-- ── Field Factories ────────────────────────────────────────────
--
-- `crap.fields.*` factory functions set `type` automatically and return
-- a plain table — fully backward compatible with raw `{ type = "text", ... }`.
-- The per-type config classes below give precise autocomplete: only the
-- properties relevant to that field type appear.

--- Shared base for all field factory configs. You never use this directly —
--- use the per-type classes via `crap.fields.text()`, `crap.fields.select()`, etc.
--- @class crap.BaseField
--- @field name          string            Column name (required).
--- @field required?     boolean           Validation: must have a value (default: false).
--- @field unique?       boolean           Unique constraint (default: false).
--- @field index?        boolean           Create a B-tree index on this column (default: false). Skipped when unique=true.
--- @field localized?    boolean           Per-locale values (default: false).
--- @field default_value? any              Default value on create.
--- @field validate?     string            Lua function ref called as `crap.ValidateFunction`.
--- @field hooks?        crap.FieldHooks   Per-field lifecycle hooks.
--- @field access?       crap.FieldAccess  Field-level access control (read/create/update).
--- @field admin?        crap.FieldAdmin   Admin UI display options.

--- @class crap.TextField : crap.BaseField
--- @field min_length?  integer  Minimum string length. Validated server-side + HTML minlength.
--- @field max_length?  integer  Maximum string length. Validated server-side + HTML maxlength.
--- @field has_many?    boolean  Multi-value tag input. Stored as JSON array in TEXT column.

--- @class crap.NumberField : crap.BaseField
--- @field min?      number   Minimum value. Validated server-side + HTML min attr.
--- @field max?      number   Maximum value. Validated server-side + HTML max attr.
--- @field has_many? boolean  Multi-value tag input. Stored as JSON array.

--- @class crap.TextareaField : crap.BaseField
--- @field min_length?  integer  Minimum string length.
--- @field max_length?  integer  Maximum string length.

--- @class crap.RichtextField : crap.BaseField

--- @class crap.SelectField : crap.BaseField
--- @field options   crap.SelectOption[]  Option list (required).
--- @field has_many? boolean              Multi-select (default: false).

--- @class crap.RadioField : crap.BaseField
--- @field options crap.SelectOption[]  Option list (required).

--- @class crap.CheckboxField : crap.BaseField

--- @class crap.DateField : crap.BaseField
--- @field picker_appearance? crap.PickerAppearance  Input type: "dayOnly" (default), "dayAndTime", "timeOnly", "monthOnly".
--- @field min_date?          string                 Minimum date (ISO "YYYY-MM-DD").
--- @field max_date?          string                 Maximum date (ISO "YYYY-MM-DD").

--- @class crap.EmailField : crap.BaseField

--- @class crap.JsonField : crap.BaseField

--- @class crap.CodeField : crap.BaseField

--- @class crap.RelationshipField : crap.BaseField
--- @field relationship crap.RelationshipConfig  Target collection and cardinality (required).

--- @class crap.UploadField : crap.BaseField
--- @field relationship? crap.RelationshipConfig  Target upload collection and cardinality.

--- @class crap.ArrayField : crap.BaseField
--- @field fields    crap.FieldDefinition[]  Sub-field definitions (required).
--- @field min_rows? integer                 Minimum rows. Validated on create/update.
--- @field max_rows? integer                 Maximum rows. Admin disables "Add" at max.

--- @class crap.GroupField : crap.BaseField
--- @field fields crap.FieldDefinition[]  Sub-field definitions (required).

--- @class crap.BlocksField : crap.BaseField
--- @field blocks    crap.BlockDefinition[]  Block type definitions (required).
--- @field min_rows? integer                 Minimum rows.
--- @field max_rows? integer                 Maximum rows.

--- @class crap.RowField : crap.BaseField
--- @field fields crap.FieldDefinition[]  Sub-field definitions (required). Promoted to parent level (no prefix).

--- @class crap.CollapsibleField : crap.BaseField
--- @field fields crap.FieldDefinition[]  Sub-field definitions (required). Promoted to parent level (no prefix).

--- @class crap.TabsField : crap.BaseField
--- @field tabs crap.FieldTab[]  Tab definitions (required). Each tab has a label and fields.

--- @class crap.JoinField : crap.BaseField
--- @field collection string  Target collection slug (required).
--- @field on         string  Field on target collection that references this document (required).

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

--- Create a richtext field (ProseMirror editor, stored as HTML by default or JSON).
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

--- Create a code field (CodeMirror editor). Set `admin.language` for syntax mode.
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


-- ── Collection Types ─────────────────────────────────────────

--- @class crap.Labels
--- @field singular? crap.LocalizedString  Singular display name (e.g., "Post" or `{ en = "Post", de = "Beitrag" }`).
--- @field plural?   crap.LocalizedString  Plural display name (e.g., "Posts" or `{ en = "Posts", de = "Beiträge" }`).

--- @class crap.AdminConfig
--- @field use_as_title?           string    Field name to use as row label in lists.
--- @field default_sort?           string    Default sort field (prefix with "-" for desc).
--- @field hidden?                 boolean   Hide from admin sidebar (default: false).
--- @field list_searchable_fields? string[]  Fields searchable in the list view.

--- @class crap.Hooks
--- @field before_validate? string[] Hook refs to run before field validation.
--- @field before_change?   string[] Hook refs to run before create/update write.
--- @field after_change?    string[] Hook refs to run after create/update write.
--- @field before_read?     string[] Hook refs to run before returning read results.
--- @field after_read?      string[] Hook refs to run after read, before response.
--- @field before_delete?   string[] Hook refs to run before delete.
--- @field after_delete?    string[] Hook refs to run after delete.
--- @field before_broadcast? string[] Hook refs to run before broadcasting live update events. Can suppress or transform event data. No CRUD access.

--- @class crap.Access
--- @field read?   string Hook ref for read access control.
--- @field create? string Hook ref for create access control.
--- @field update? string Hook ref for update access control.
--- @field delete? string Hook ref for delete access control.

--- @class crap.AuthStrategyContext
--- @field headers    table<string, string>  Request headers (lowercase keys).
--- @field collection string                 Auth collection slug.

--- @class crap.AuthStrategy
--- @field name          string  Strategy name (e.g., "api-key", "ldap").
--- @field authenticate  string  Lua function ref (module.function format). Receives `crap.AuthStrategyContext`, returns a user document or nil.

--- @class crap.Auth
--- @field enabled?          boolean             Enable auth for this collection (default: false).
--- @field token_expiry?     integer             JWT token expiry in seconds (default: 7200).
--- @field strategies?       crap.AuthStrategy[] Custom auth strategies for request-level authentication.
--- @field disable_local?    boolean             Disable local password login (default: false). When true, only custom strategies can authenticate.
--- @field verify_email?     boolean             Require email verification before login (default: false). Sends a verification email on user create.
--- @field forgot_password?  boolean             Enable forgot password flow (default: true). Sends a reset email when requested.

--- @alias crap.ImageFit "cover" | "contain" | "inside" | "fill"

--- @class crap.ImageSize
--- @field name   string       Size name (e.g., "thumbnail", "card").
--- @field width  integer      Target width in pixels.
--- @field height integer      Target height in pixels.
--- @field fit?   crap.ImageFit  Resize fit mode (default: "cover").

--- @class crap.FormatQuality
--- @field quality integer  Encoding quality 1-100.
--- @field queue?  boolean  Defer conversion to background queue (default: false).

--- @class crap.FormatOptions
--- @field webp? crap.FormatQuality  Auto-generate WebP variant for each size.
--- @field avif? crap.FormatQuality  Auto-generate AVIF variant for each size.

--- @class crap.CollectionUpload
--- @field mime_types?      string[]            MIME type allowlist with glob support (e.g., "image/*"). Empty = any type.
--- @field max_file_size?   integer|string       Max file size — bytes (integer) or human-readable ("10MB", "1GB"). Overrides global default.
--- @field image_sizes?     crap.ImageSize[]    Resize definitions for image uploads.
--- @field admin_thumbnail? string              Name of image_size to show in admin list.
--- @field format_options?  crap.FormatOptions  Auto-generate format variants for each size.

--- @class crap.VersionsConfig
--- @field drafts?       boolean  Enable draft/publish workflow (default: false). Adds `_status` column.
--- @field max_versions? integer  Maximum version snapshots to keep per document (default: unlimited).

--- @class crap.CollectionConfig
--- @field labels?     crap.Labels      Display names.
--- @field timestamps? boolean                    Auto created_at/updated_at (default: true).
--- @field auth?       boolean|crap.Auth  Enable authentication on this collection. `true` for defaults, or a config table with strategies/token_expiry/disable_local.
--- @field upload?     boolean|crap.CollectionUpload  Enable file uploads. `true` for defaults, or a config table with mime_types/max_file_size/image_sizes.
--- @field fields?     crap.FieldDefinition[]     Field definitions.
--- @field admin?      crap.AdminConfig       Admin UI options.
--- @field hooks?      crap.Hooks       Hook references.
--- @field access?     crap.Access      Access control function refs.
--- @field versions?   boolean|crap.VersionsConfig  Enable versioning. `true` for defaults, or a config table with drafts/max_versions.
--- @field live?       boolean|string              Live event broadcasting. `false` to disable, string for Lua function ref that receives `{ collection, operation, data }` and returns boolean. Absent = enabled (broadcast all).
--- @field indexes?    crap.IndexDefinition[]      Compound indexes (multi-column). Created on startup, stale indexes dropped.
--- @field mcp?        crap.McpCollectionConfig    MCP tool description and options.

--- @class crap.IndexDefinition
--- @field fields string[]    Column names to include in the index (required).
--- @field unique? boolean    Create a UNIQUE index (default: false).


-- ── Global Types ─────────────────────────────────────────────

--- @class crap.GlobalConfig
--- @field labels?    crap.Labels    Display names.
--- @field fields?    crap.FieldDefinition[]   Field definitions.
--- @field hooks?     crap.Hooks     Hook references.
--- @field access?    crap.Access    Access control function refs.
--- @field versions?  boolean|crap.VersionsConfig  Enable versioning. Same as collection `versions`.
--- @field live?      boolean|string           Live event broadcasting. Same as collection `live`.
--- @field mcp?       crap.McpCollectionConfig  MCP tool description and options.


-- ── Document Types ───────────────────────────────────────────

--- @class crap.Document
--- @field id         string               Unique document ID (nanoid).
--- @field [string]   any                  Dynamic fields from the collection schema.
--- @field created_at? string              ISO 8601 timestamp (if timestamps enabled).
--- @field updated_at? string              ISO 8601 timestamp (if timestamps enabled).

--- Filter operator table. Use one key per operator.
--- Simple string values are treated as `equals`.
---
--- Example:
--- ```lua
--- crap.collections.find("posts", {
---     where = {
---         status = "published",                     -- shorthand for equals
---         title = { contains = "hello" },
---         created_at = { greater_than = "2024-01-01" },
---         category = { ["in"] = { "news", "blog" } },
---     }
--- })
--- ```
--- @class crap.FilterOperators
--- @field equals?                string   Exact match (field = value).
--- @field not_equals?            string   Not equal (field != value).
--- @field like?                  string   SQL LIKE pattern (field LIKE value).
--- @field contains?              string   Substring match (field LIKE %value%).
--- @field greater_than?          string   Greater than (field > value).
--- @field less_than?             string   Less than (field < value).
--- @field greater_than_or_equal? string   Greater than or equal (field >= value).
--- @field less_than_or_equal?    string   Less than or equal (field <= value).
--- @field ["in"]?                string[] Value in list (field IN (...)).
--- @field not_in?                string[] Value not in list (field NOT IN (...)).
--- @field exists?                boolean  Field is not null (IS NOT NULL).
--- @field not_exists?            boolean  Field is null (IS NULL).

--- @alias crap.FilterValue string|crap.FilterOperators

--- @class crap.OrCondition : table<string, crap.FilterValue>
--- A single OR group — an object of field filters that are AND-ed together.

--- @class crap.FindQuery
--- @field where?           table<string, crap.FilterValue>  Field filters. String values = equals, table values = operators. Use `["or"]` key for OR groups. Keys support dot notation for nested fields: `"seo.title"` (group), `"variants.color"` (array sub-field), `"content.body"` (block sub-field), `"content._block_type"` (block type), `"tags.id"` (has-many relationship).
--- @field order_by?       string                 Sort field (prefix with "-" for desc).
--- @field limit?          integer                Max results to return.
--- @field page?           integer                Page number (1-based). Converted to offset internally.
--- @field offset?         integer                Number of results to skip (backward compat alias for page).
--- @field depth?          integer                Population depth for relationship fields (default: 0). 0 = IDs only.
--- @field locale?         string                 Locale code for localized fields (e.g., "en", "de", "all"). Nil = default locale.
--- @field select?         string[]               Fields to return. Nil/empty = all fields. Always includes id, created_at, updated_at.
--- @field draft?          boolean                Include draft documents (versioned collections only). Default: false.
--- @field overrideAccess? boolean                Skip access control checks (default: true). Set to false to enforce collection-level and field-level access for the current user.
--- @field after_cursor?  string                 Forward cursor token for keyset pagination (from previous response's `endCursor`). Mutually exclusive with `page`/`offset`/`before_cursor`.
--- @field before_cursor? string                 Backward cursor token for keyset pagination (from previous response's `startCursor`). Mutually exclusive with `page`/`offset`/`after_cursor`.
--- @field search?        string                 FTS5 full-text search query. Filters results to documents matching this search term via the FTS5 index. Indexed fields are determined by `list_searchable_fields` or auto-detected text-like fields.

--- @class crap.PaginationInfo
--- @field totalDocs    integer   Total matching documents (before limit/page).
--- @field limit        integer   Applied limit for this query.
--- @field totalPages?  integer   Total number of pages (offset mode only).
--- @field page?        integer   Current page number (offset mode only, 1-based).
--- @field pageStart?   integer   1-based index of the first document on the current page (offset mode only).
--- @field hasPrevPage  boolean   Whether a previous page exists.
--- @field hasNextPage  boolean   Whether a next page exists.
--- @field prevPage?    integer   Previous page number (offset mode only, nil if on first page).
--- @field nextPage?    integer   Next page number (offset mode only, nil if on last page).
--- @field startCursor? string    Opaque cursor of the first document in results (cursor mode only).
--- @field endCursor?   string    Opaque cursor of the last document in results (cursor mode only).

--- @class crap.FindResult
--- @field documents  crap.Document[]     Matching documents.
--- @field pagination crap.PaginationInfo  Pagination metadata.


-- ── Hook Context Types ───────────────────────────────────────

--- @class crap.HookContext
--- @field collection string                 Collection slug.
--- @field operation  "create"|"update"|"delete"|"find"|"find_by_id"|"get_global"|"init"  The operation being performed.
--- @field data       table<string, any>     Document data (mutable in before_* hooks). For read hooks, contains document fields including id/timestamps. For delete hooks, contains only `{ id = "..." }`. In after_change hooks, `data.id` contains the new document ID.
--- @field locale?    string                 Current locale code (nil if localization disabled or default locale).
--- @field context    table<string, any>     Request-scoped shared table. Persists from before_validate through after_change within one request. Only JSON-compatible values survive (no functions/userdata).
--- @field hook_depth integer                Current recursion depth. 0 = top-level API/admin call, 1+ = from Lua CRUD inside hooks. Hooks are skipped when this reaches `hooks.max_depth` (default: 3).
--- @field draft?     boolean                True when this is a draft save (only set for collections with `versions.drafts` enabled).
--- @field user?      table                  Authenticated user document (nil if unauthenticated or no auth collection). Contains all fields of the user's auth collection document.
--- @field ui_locale? string                 Admin UI locale code (e.g., "en", "de"). Nil if not set or called from gRPC without locale context.

--- @class crap.FieldAccess
--- @field read?   string Hook ref for field read access control.
--- @field create? string Hook ref for field create access control.
--- @field update? string Hook ref for field update access control.

--- Access function context. Passed to collection-level and field-level access functions.
--- Return `true` to allow, `false`/`nil` to deny, or a filter table (read only)
--- to allow with query constraints.
---
--- Filter table format (same as `crap.collections.find()` `where`):
--- ```lua
--- function M.own_or_admin(ctx)
---     if ctx.user == nil then return false end
---     if ctx.user.role == "admin" then return true end
---     return { created_by = ctx.user.id }  -- merged into query as AND clause
--- end
--- ```
--- @class crap.AccessContext
--- @field user? crap.Document      Full user document from auth collection (nil if anonymous).
--- @field id?   string             Document ID (for update/delete/find_by_id).
--- @field data? table<string, any> Incoming data (for create/update).


-- ── crap.collections ─────────────────────────────────────────

--- Collection definition and runtime CRUD operations.
--- @class crap.collections
crap.collections = {}

--- Define a new collection. Call this in collection definition files.
--- @param slug string   Unique collection identifier (used in URLs and DB).
--- @param config crap.CollectionConfig  Collection configuration.
function crap.collections.define(slug, config) end

--- Schema introspection sub-table for collections.
--- @class crap.collections.config
crap.collections.config = {}

--- Get a collection's current definition as a Lua table.
--- Returns the full config compatible with `define()` for round-trip editing.
--- @param slug string  Collection slug.
--- @return crap.CollectionConfig?  The collection config, or nil if not found.
function crap.collections.config.get(slug) end

--- List all registered collections as a slug-keyed table of full configs.
--- Iterate with `for slug, def in pairs(crap.collections.config.list()) do ... end`.
--- @return table<string, crap.CollectionConfig>  Slug -> collection config map.
function crap.collections.config.list() end

--- Find documents matching a query. Returns documents and total count.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string        Collection slug.
--- @param query?     crap.FindQuery Query parameters.
--- @return crap.FindResult
function crap.collections.find(collection, query) end

--- @class crap.FindByIdOptions
--- @field depth?          integer   Population depth for relationship fields (default: 0). 0 = IDs only.
--- @field locale?         string    Locale code for localized fields (e.g., "en", "de", "all"). Nil = default locale.
--- @field select?         string[]  Fields to return. Nil/empty = all fields. Always includes id.
--- @field draft?          boolean   When true and the collection has `versions.drafts`, returns the latest draft version snapshot instead of the published main-table data. Default: false.
--- @field overrideAccess? boolean   Skip access control checks (default: true). Set to false to enforce collection-level and field-level access for the current user.

--- Find a single document by ID.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param id         string  Document ID.
--- @param opts?      crap.FindByIdOptions  Optional options (e.g., `{ depth = 1 }`).
--- @return crap.Document?
function crap.collections.find_by_id(collection, id, opts) end

--- @class crap.CreateOptions
--- @field locale?         string   Locale code for localized fields. Nil = default locale.
--- @field overrideAccess? boolean  Skip access control checks (default: true). Set to false to enforce collection-level and field-level access for the current user.
--- @field draft?          boolean  When true and the collection has `versions.drafts`, creates the document with `_status = 'draft'` and skips required-field validation.
--- @field hooks?          boolean  Run lifecycle hooks (default: true). Set false to bypass hooks (e.g., for seeding/migrations).

--- @class crap.UpdateOptions
--- @field locale?         string   Locale code for localized fields. Nil = default locale.
--- @field overrideAccess? boolean  Skip access control checks (default: true). Set to false to enforce collection-level and field-level access for the current user.
--- @field draft?          boolean  When true and the collection has `versions.drafts`, performs a version-only save (main table unchanged, only a draft version snapshot is created).
--- @field hooks?          boolean  Run lifecycle hooks (default: true). Set false to bypass hooks.
--- @field unpublish?      boolean  When true and the collection has `versions`, sets `_status` to `"draft"` (unpublishes). Data is not modified.

--- Create a new document.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string           Collection slug.
--- @param data       table<string, any> Field values.
--- @param opts?      crap.CreateOptions  Optional options (e.g., `{ locale = "de" }`).
--- @return crap.Document
function crap.collections.create(collection, data, opts) end

--- Update an existing document.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string           Collection slug.
--- @param id         string           Document ID.
--- @param data       table<string, any> Fields to update (partial).
--- @param opts?      crap.UpdateOptions  Optional options (e.g., `{ locale = "de" }`).
--- @return crap.Document
function crap.collections.update(collection, id, data, opts) end

--- @class crap.DeleteOptions
--- @field overrideAccess? boolean  Skip access control checks (default: true). Set to false to enforce collection-level access for the current user.
--- @field hooks?          boolean  Run lifecycle hooks (default: true). Set false to bypass hooks.

--- Delete a document.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param id         string  Document ID.
--- @param opts?      crap.DeleteOptions  Optional options.
--- @return boolean success
function crap.collections.delete(collection, id, opts) end

--- @class crap.CountQuery
--- @field where?          table<string, crap.FilterValue>  Field filters. Supports dot notation for nested fields (same as FindQuery).
--- @field locale?         string                 Locale code for localized fields.
--- @field overrideAccess? boolean                Skip access control checks (default: true).
--- @field draft?          boolean                Include draft documents (default: false).
--- @field search?         string                 FTS5 full-text search query (same as FindQuery).

--- Count documents matching a query.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string        Collection slug.
--- @param query?     crap.CountQuery Query parameters.
--- @return integer count
function crap.collections.count(collection, query) end

--- @class crap.UpdateManyQuery
--- @field where?          table<string, crap.FilterValue>  Field filters to match documents. Supports dot notation for nested fields (same as FindQuery).
--- @field locale?         string                 Locale code for localized fields.
--- @field overrideAccess? boolean                Skip access control checks (default: true).
--- @field draft?          boolean                Include draft documents (default: false).

--- Update multiple documents matching a query. All-or-nothing: checks update access
--- for every matched document first. If any fails, returns error and nothing is modified.
--- Does NOT fire per-document hooks.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string                  Collection slug.
--- @param query      crap.UpdateManyQuery    Query to match documents.
--- @param data       table<string, any>      Fields to update on all matched documents.
--- @param opts?      crap.UpdateOptions      Optional options.
--- @return { modified: integer }
function crap.collections.update_many(collection, query, data, opts) end

--- @class crap.DeleteManyQuery
--- @field where?          table<string, crap.FilterValue>  Field filters to match documents. Supports dot notation for nested fields (same as FindQuery).
--- @field locale?         string                 Locale code for localized fields.
--- @field overrideAccess? boolean                Skip access control checks (default: true).

--- Delete multiple documents matching a query. All-or-nothing: checks delete access
--- for every matched document first. If any fails, returns error and nothing is modified.
--- Does NOT fire per-document hooks.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string                  Collection slug.
--- @param query      crap.DeleteManyQuery    Query to match documents.
--- @return { deleted: integer }
function crap.collections.delete_many(collection, query) end


-- ── crap.globals ─────────────────────────────────────────────

--- Global (singleton document) definition and runtime operations.
--- @class crap.globals
crap.globals = {}

--- Define a new global. Call this in global definition files.
--- @param slug   string            Unique global identifier.
--- @param config crap.GlobalConfig Global configuration.
function crap.globals.define(slug, config) end

--- Schema introspection sub-table for globals.
--- @class crap.globals.config
crap.globals.config = {}

--- Get a global's current definition as a Lua table.
--- Returns the full config compatible with `define()` for round-trip editing.
--- @param slug string  Global slug.
--- @return crap.GlobalConfig?  The global config, or nil if not found.
function crap.globals.config.get(slug) end

--- List all registered globals as a slug-keyed table of full configs.
--- Iterate with `for slug, def in pairs(crap.globals.config.list()) do ... end`.
--- @return table<string, crap.GlobalConfig>  Slug -> global config map.
function crap.globals.config.list() end

--- @class crap.GlobalGetOptions
--- @field locale? string  Locale code for localized fields. Nil = default locale.

--- @class crap.GlobalUpdateOptions
--- @field locale? string  Locale code for localized fields. Nil = default locale.

--- Get a global's current value.
--- @param slug string  Global slug.
--- @param opts? crap.GlobalGetOptions  Optional options (e.g., `{ locale = "de" }`).
--- @return crap.Document
function crap.globals.get(slug, opts) end

--- Update a global's value.
--- @param slug string              Global slug.
--- @param data table<string, any>  Fields to update.
--- @param opts? crap.GlobalUpdateOptions  Optional options (e.g., `{ locale = "de" }`).
--- @return crap.Document
function crap.globals.update(slug, data, opts) end


-- ── crap.hooks ───────────────────────────────────────────────

--- Hook registration API.
--- @class crap.hooks
crap.hooks = {}

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

--- Register a hook function for an event. Fires for all collections.
--- @param event crap.HookEvent  The lifecycle event to hook into.
--- @param fn    fun(context: crap.HookContext): crap.HookContext  Hook function.
function crap.hooks.register(event, fn) end

--- Remove a previously registered hook function (identity-based via rawequal).
--- @param event crap.HookEvent  The lifecycle event.
--- @param fn    fun(context: crap.HookContext): crap.HookContext  The function to remove.
function crap.hooks.remove(event, fn) end


-- ── crap.richtext ────────────────────────────────────────────

--- Custom ProseMirror node registration and rendering.
--- @class crap.richtext
crap.richtext = {}

--- Attribute type for custom richtext node attributes.
--- @alias crap.NodeAttrType "text"|"number"|"select"|"checkbox"|"textarea"

--- A single attribute on a custom richtext node.
--- @class crap.NodeAttr
--- @field name     string              Attribute name.
--- @field type     crap.NodeAttrType   Input type in admin editor.
--- @field label?   string              Display label (defaults to name).
--- @field required? boolean            Whether the attribute is required (default: false).
--- @field default?  any                Default value.
--- @field options?  crap.SelectOption[] Options for select-type attributes.

--- Spec for registering a custom richtext node.
--- @class crap.RichtextNodeSpec
--- @field label?           string          Display label (defaults to name).
--- @field inline?          boolean         Whether the node is inline (default: false = block).
--- @field attrs?           crap.NodeAttr[] Attribute definitions.
--- @field searchable_attrs? string[]       Attr names to include in FTS search index.
--- @field render?          fun(attrs: table): string  Server-side render function.

--- Register a custom ProseMirror node type.
--- @param name string  Node name (alphanumeric + underscores only).
--- @param spec crap.RichtextNodeSpec  Node specification.
function crap.richtext.register_node(name, spec) end

--- Render richtext content, replacing custom nodes with their rendered HTML.
--- Detects format automatically: starts with '{' = JSON, otherwise HTML.
--- @param content string  Richtext content (HTML or ProseMirror JSON).
--- @return string html  Rendered HTML output.
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


-- ── crap.util ────────────────────────────────────────────────

--- Utility functions.
--- @class crap.util
crap.util = {}

--- Generate a URL-safe slug from a string.
--- @param str string  Input string.
--- @return string slug  Lowercased, hyphenated slug.
function crap.util.slugify(str) end

--- Generate a unique nanoid.
--- @return string id  Random nanoid string.
function crap.util.nanoid() end

--- Encode a Lua table as a JSON string.
--- @param value any  Lua value to encode.
--- @return string json  JSON string.
function crap.util.json_encode(value) end

--- Decode a JSON string into a Lua table.
--- @param str string  JSON string.
--- @return any value  Decoded Lua value.
function crap.util.json_decode(str) end

-- ── crap.util — table helpers ──────────────────────────────

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

-- ── crap.util — string helpers ─────────────────────────────

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

-- ── crap.util — date helpers ───────────────────────────────

--- Get current UTC time as ISO 8601 string.
--- @return string
function crap.util.date_now() end

--- Get current Unix timestamp in seconds.
--- @return integer
function crap.util.date_timestamp() end

--- Parse a date string to Unix timestamp. Tries RFC 3339, then common formats.
--- Throws an error if the string cannot be parsed.
--- @param str string  Date string to parse.
--- @return integer timestamp  Unix seconds.
function crap.util.date_parse(str) end

--- Format a Unix timestamp using a format string (chrono syntax).
--- Throws an error if the timestamp or format is invalid.
--- @param timestamp integer  Unix seconds.
--- @param format string      Format string (e.g., "%Y-%m-%d %H:%M:%S").
--- @return string
function crap.util.date_format(timestamp, format) end

--- Add seconds to a timestamp.
--- @param timestamp integer  Unix seconds.
--- @param seconds integer    Seconds to add.
--- @return integer
function crap.util.date_add(timestamp, seconds) end

--- Difference between two timestamps in seconds.
--- @param a integer  First timestamp.
--- @param b integer  Second timestamp.
--- @return integer  a - b in seconds.
function crap.util.date_diff(a, b) end


-- ── crap.auth ──────────────────────────────────────────────────

--- Password hashing and verification helpers.
--- @class crap.auth
crap.auth = {}

--- Hash a plaintext password (Argon2id).
--- @param password string  Plaintext password.
--- @return string hash  Hashed password.
function crap.auth.hash_password(password) end

--- Verify a password against a hash.
--- @param password string  Plaintext password.
--- @param hash     string  Stored hash.
--- @return boolean valid
function crap.auth.verify_password(password, hash) end


-- ── crap.env ─────────────────────────────────────────────────

--- Read-only access to environment variables.
--- @class crap.env
crap.env = {}

--- Get an environment variable.
--- @param key string  Variable name.
--- @return string?  Value or nil if not set.
function crap.env.get(key) end


-- ── crap.http ────────────────────────────────────────────────

--- Outbound HTTP client (blocking, runs inside spawn_blocking context).
--- @class crap.http
crap.http = {}

--- @class crap.HttpRequest
--- @field url      string            Request URL (required).
--- @field method?  string            HTTP method (default: "GET").
--- @field headers? table<string, string>  Request headers.
--- @field body?    string            Request body.
--- @field timeout? integer           Request timeout in seconds (default: 30).

--- @class crap.HttpResponse
--- @field status  integer           HTTP status code.
--- @field headers table<string, string>  Response headers.
--- @field body    string            Response body.

--- Make an outbound HTTP request.
--- @param opts crap.HttpRequest  Request options.
--- @return crap.HttpResponse
function crap.http.request(opts) end


-- ── crap.config ──────────────────────────────────────────────

--- Email sending (requires SMTP configuration in crap.toml).
--- @class crap.email
crap.email = {}

--- @class crap.EmailOptions
--- @field to      string  Recipient email address (required).
--- @field subject string  Email subject line (required).
--- @field html    string  HTML email body (required).
--- @field text?   string  Plain text fallback body.

--- Send an email via SMTP. Blocking — safe to call from hooks.
--- Returns true on success. If email is not configured (smtp_host empty), logs a warning and returns true (no-op).
--- @param opts crap.EmailOptions  Email options.
--- @return boolean success
function crap.email.send(opts) end


--- Read-only access to crap.toml configuration values.
--- Values are a snapshot from startup — changes to crap.toml after
--- startup won't be reflected until restart.
--- @class crap.config
crap.config = {}

--- Get a configuration value using dot notation.
--- @param key string  Dot-separated config key (e.g., "server.admin_port").
--- @return any
function crap.config.get(key) end


-- ── crap.locale ─────────────────────────────────────────────

--- Locale configuration access (read-only).
--- Available in init.lua and hook functions.
--- @class crap.locale
crap.locale = {}

--- Get the default locale code (e.g., "en").
--- @return string
function crap.locale.get_default() end

--- Get all configured locale codes (e.g., {"en", "de", "fr"}).
--- Returns an empty table if localization is disabled.
--- @return string[]
function crap.locale.get_all() end

--- Check if localization is enabled (at least one locale configured).
--- @return boolean
function crap.locale.is_enabled() end


-- ── crap.crypto ────────────────────────────────────────────

--- Cryptographic helpers. Keys derived from the auth secret in crap.toml.
--- @class crap.crypto
crap.crypto = {}

--- SHA-256 hash of a string, returned as hex.
--- @param data string  Data to hash.
--- @return string hex  64-character hex string.
function crap.crypto.sha256(data) end

--- HMAC-SHA256 of data with a key, returned as hex.
--- @param data string  Data to authenticate.
--- @param key  string  HMAC key.
--- @return string hex  64-character hex string.
function crap.crypto.hmac_sha256(data, key) end

--- Base64 encode a string.
--- @param str string
--- @return string
function crap.crypto.base64_encode(str) end

--- Base64 decode a string.
--- @param str string
--- @return string
function crap.crypto.base64_decode(str) end

--- Encrypt plaintext using AES-256-GCM. Key derived from auth secret.
--- Returns base64-encoded ciphertext (nonce prepended).
--- @param plaintext string
--- @return string ciphertext  Base64-encoded.
function crap.crypto.encrypt(plaintext) end

--- Decrypt ciphertext produced by `encrypt`.
--- @param ciphertext string  Base64-encoded ciphertext from `encrypt`.
--- @return string plaintext
function crap.crypto.decrypt(ciphertext) end

--- Generate random bytes as hex string.
--- @param n integer  Number of random bytes.
--- @return string hex  Hex string of length 2*n.
function crap.crypto.random_bytes(n) end


-- ── crap.schema ────────────────────────────────────────────

--- Schema introspection API (read-only). Reads from the loaded registry.
--- @class crap.schema
crap.schema = {}

--- @class crap.SchemaCollection
--- @field slug        string
--- @field labels      { singular?: string, plural?: string }
--- @field timestamps  boolean
--- @field has_auth    boolean
--- @field has_upload  boolean
--- @field has_versions boolean
--- @field has_drafts  boolean
--- @field fields      crap.SchemaField[]

--- @class crap.SchemaField
--- @field name         string
--- @field type         string
--- @field required     boolean
--- @field localized    boolean
--- @field unique       boolean
--- @field relationship? { collection: string, has_many: boolean, max_depth?: integer }
--- @field options?     { label: string, value: string }[]
--- @field fields?      crap.SchemaField[]
--- @field blocks?      { block_type: string, label?: string, group?: string, image_url?: string, fields: crap.SchemaField[] }[]

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


-- ── crap.jobs ─────────────────────────────────────────────

--- Background job definition and queuing API.
--- @class crap.jobs
crap.jobs = {}

--- @class crap.JobLabels
--- @field singular? string  Display label for the job (e.g., "Cleanup Expired Posts").

--- @class crap.JobDefinitionConfig
--- @field handler         string            Lua function ref for the job handler (required, e.g., "jobs.cleanup.run").
--- @field schedule?       string            Cron expression (e.g., "0 3 * * *"). If set, job runs on this schedule.
--- @field queue?          string            Queue name (default: "default").
--- @field retries?        integer           Max retry attempts on failure (default: 0).
--- @field timeout?        integer           Seconds before a running job is marked failed (default: 60).
--- @field concurrency?    integer           Max concurrent runs of this job (default: 1).
--- @field skip_if_running? boolean          Skip scheduled run if previous still running (default: true).
--- @field labels?         crap.JobLabels    Display labels for admin UI.
--- @field access?         string            Lua function ref for access control on gRPC/CLI trigger.

--- @class crap.JobHandlerContext
--- @field data table<string, any>  Input data from queue() or {} for cron.
--- @field job  crap.JobInfo        Job metadata.

--- @class crap.JobInfo
--- @field slug         string   Job definition slug.
--- @field attempt      integer  Current attempt number (1-based).
--- @field max_attempts integer  Total max attempts.

--- Define a background job. Call in init.lua or jobs/*.lua files.
--- The handler function receives a context table with `data` and `job` fields,
--- and has full CRUD access (crap.collections.find/create/update/delete).
--- @param slug   string                    Unique job identifier.
--- @param config crap.JobDefinitionConfig  Job configuration.
function crap.jobs.define(slug, config) end

--- Queue a job for background execution. Returns the job run ID.
--- Only available inside hooks with transaction context.
--- @param slug string                     Job slug (must be previously defined).
--- @param data? table<string, any>        Input data passed to the handler (default: {}).
--- @return string job_id  The queued job run ID.
function crap.jobs.queue(slug, data) end
