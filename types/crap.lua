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
--- | "richtext"     # Rich text (stored as JSON)
--- | "select"       # Single select from options
--- | "checkbox"     # Boolean (true/false)
--- | "date"         # ISO 8601 date/datetime
--- | "email"        # Validated email address
--- | "json"         # Arbitrary JSON blob
--- | "upload"       # File upload (references media collection)
--- | "relationship" # Reference to another collection
--- | "slug"         # Auto-generated URL slug
--- | "array"        # Repeatable sub-fields
--- | "group"        # Visual grouping (no extra table)
--- | "blocks"       # Flexible content blocks
--- | "point"        # GeoJSON point [lng, lat]

--- @alias crap.FieldWidth "full" | "half" | "third"

--- @class crap.RelationshipConfig
--- @field collection string   Target collection slug (required).
--- @field has_many?  boolean  Many-to-many relationship via junction table (default: false).
--- @field max_depth? integer  Per-field max population depth. Limits depth regardless of request-level depth.

--- @class crap.SelectOption
--- @field label string Display text in the admin UI.
--- @field value string Stored value.

--- @class crap.FieldAdmin
--- @field label?       string   UI label (defaults to field name).
--- @field description? string   Help text shown below the input.
--- @field placeholder? string   Input placeholder text.
--- @field hidden?      boolean  Hide from admin UI (default: false).
--- @field readonly?    boolean  Non-editable in admin (default: false).
--- @field width?       crap.FieldWidth  Field width: "full", "half", or "third".
--- @field position?    string   "main" or "sidebar".
--- @field condition?   string   Lua function ref for conditional show/hide.
--- @field components?  table    Override admin UI partials for this field.

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
---
--- Note: before_validate and before_change field hooks have full CRUD access
--- (crap.collections.find/create/update/delete) via the parent transaction.
--- after_change and after_read field hooks do NOT have CRUD access (fire-and-forget).

--- @class crap.BlockDefinition
--- @field type   string                    Block type identifier (required).
--- @field label? string                    Display label for the block type (defaults to type name).
--- @field fields crap.FieldDefinition[]    Fields within this block type.

--- @class crap.FieldDefinition
--- @field name          string            Column name (required).
--- @field type          crap.FieldType    Field type (required).
--- @field required?     boolean           Validation: must have a value (default: false).
--- @field unique?       boolean           Unique constraint (default: false).
--- @field localized?    boolean           Per-locale values (default: false).
--- @field default_value? any              Default value on create.
--- @field hidden?       boolean           Hide from API responses (default: false).
--- @field validate?     string            Lua function ref (module.function format) called as crap.ValidateFunction.
--- @field hooks?        crap.FieldHooks   Per-field lifecycle hooks (value transformers, no CRUD access).
--- @field options?      crap.SelectOption[] Options for "select" field type.
--- @field relationship? crap.RelationshipConfig  Relationship config (preferred syntax).
--- @field relation_to?  string            Target collection (legacy flat syntax).
--- @field has_many?     boolean           Many-to-many relationship (legacy flat syntax, default: false).
--- @field fields?       crap.FieldDefinition[] Sub-fields for "array" / "group" types.
--- @field blocks?       crap.BlockDefinition[] Block type definitions for "blocks" type.
--- @field admin?        crap.FieldAdmin   Admin UI display options.
--- @field access?       crap.FieldAccess  Field-level access control (read/create/update).


-- ── Collection Types ─────────────────────────────────────────

--- @class crap.CollectionLabels
--- @field singular? string  Singular display name (e.g., "Post").
--- @field plural?   string  Plural display name (e.g., "Posts").

--- @class crap.CollectionAdmin
--- @field use_as_title?           string    Field name to use as row label in lists.
--- @field default_sort?           string    Default sort field (prefix with "-" for desc).
--- @field hidden?                 boolean   Hide from admin sidebar (default: false).
--- @field list_searchable_fields? string[]  Fields searchable in the list view.

--- @class crap.CollectionHooks
--- @field before_validate? string[] Hook refs to run before field validation.
--- @field before_change?   string[] Hook refs to run before create/update write.
--- @field after_change?    string[] Hook refs to run after create/update write.
--- @field before_read?     string[] Hook refs to run before returning read results.
--- @field after_read?      string[] Hook refs to run after read, before response.
--- @field before_delete?   string[] Hook refs to run before delete.
--- @field after_delete?    string[] Hook refs to run after delete.

--- @class crap.CollectionAccess
--- @field read?   string Hook ref for read access control.
--- @field create? string Hook ref for create access control.
--- @field update? string Hook ref for update access control.
--- @field delete? string Hook ref for delete access control.

--- @class crap.AuthStrategy
--- @field name          string  Strategy name (e.g., "api-key", "ldap").
--- @field authenticate  string  Lua function ref (module.function format) that receives `{ headers, collection }` and returns a user document or nil.

--- @class crap.CollectionAuth
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

--- @class crap.FormatOptions
--- @field webp? crap.FormatQuality  Auto-generate WebP variant for each size.
--- @field avif? crap.FormatQuality  Auto-generate AVIF variant for each size.

--- @class crap.CollectionUpload
--- @field mime_types?      string[]            MIME type allowlist with glob support (e.g., "image/*"). Empty = any type.
--- @field max_file_size?   integer             Max file size in bytes (overrides global default).
--- @field image_sizes?     crap.ImageSize[]    Resize definitions for image uploads.
--- @field admin_thumbnail? string              Name of image_size to show in admin list.
--- @field format_options?  crap.FormatOptions  Auto-generate format variants for each size.

--- @class crap.CollectionConfig
--- @field labels?     crap.CollectionLabels      Display names.
--- @field slug?       string                     URL segment (defaults to name).
--- @field timestamps? boolean                    Auto created_at/updated_at (default: true).
--- @field auth?       boolean|crap.CollectionAuth  Enable authentication on this collection. `true` for defaults, or a config table with strategies/token_expiry/disable_local.
--- @field upload?     boolean|crap.CollectionUpload  Enable file uploads. `true` for defaults, or a config table with mime_types/max_file_size/image_sizes.
--- @field fields?     crap.FieldDefinition[]     Field definitions.
--- @field admin?      crap.CollectionAdmin       Admin UI options.
--- @field hooks?      crap.CollectionHooks       Hook references.
--- @field access?     crap.CollectionAccess      Access control function refs.


-- ── Global Types ─────────────────────────────────────────────

--- @class crap.GlobalConfig
--- @field labels?  crap.CollectionLabels    Display names.
--- @field fields?  crap.FieldDefinition[]   Field definitions.
--- @field hooks?   crap.CollectionHooks     Hook references.
--- @field access?  crap.CollectionAccess    Access control function refs.


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
---     filters = {
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

--- @class crap.FindQuery
--- @field filters?  table<string, crap.FilterValue>  Field filters. String values = equals, table values = operators.
--- @field order_by? string                 Sort field (prefix with "-" for desc).
--- @field limit?    integer                Max results to return.
--- @field offset?   integer                Number of results to skip.
--- @field depth?    integer                Population depth for relationship fields (default: 0). 0 = IDs only.

--- @class crap.FindResult
--- @field documents crap.Document[]  Matching documents.
--- @field total     integer          Total count (before limit/offset).


-- ── Hook Context Types ───────────────────────────────────────

--- @class crap.HookContext
--- @field collection string                 Collection slug.
--- @field operation  string                 The operation: "create", "update", "delete", "find", "find_by_id", "get_global", or "init".
--- @field data       table<string, any>     Document data (mutable in before_* hooks). For read hooks, contains document fields.
--- @field original_doc? crap.Document       Original document (on update).
--- @field req?       crap.RequestContext     Request context (if available).

--- @class crap.ReadHookContext
--- @field collection string           Collection slug.
--- @field doc        crap.Document    The document being read.
--- @field req?       crap.RequestContext

--- @class crap.DeleteHookContext
--- @field collection string           Collection slug.
--- @field id         string           Document ID being deleted.
--- @field req?       crap.RequestContext

--- @class crap.RequestContext
--- @field user? crap.User  Authenticated user (nil if anonymous).

--- @class crap.FieldAccess
--- @field read?   string Hook ref for field read access control.
--- @field create? string Hook ref for field create access control.
--- @field update? string Hook ref for field update access control.

--- Access function context. Passed to collection-level and field-level access functions.
--- Return `true` to allow, `false`/`nil` to deny, or a filter table (read only)
--- to allow with query constraints.
---
--- Filter table format (same as `crap.collections.find()` filters):
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

--- @class crap.User
--- @field id    string
--- @field email string
--- @field role  string


-- ── crap.collections ─────────────────────────────────────────

--- Collection definition and runtime CRUD operations.
--- @class crap.collections
crap.collections = {}

--- Define a new collection. Call this in collection definition files.
--- @param slug string   Unique collection identifier (used in URLs and DB).
--- @param config crap.CollectionConfig  Collection configuration.
function crap.collections.define(slug, config) end

--- Find documents matching a query. Returns documents and total count.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string        Collection slug.
--- @param query?     crap.FindQuery Query parameters.
--- @return crap.FindResult
function crap.collections.find(collection, query) end

--- @class crap.FindByIdOptions
--- @field depth? integer  Population depth for relationship fields (default: 0). 0 = IDs only.

--- Find a single document by ID.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param id         string  Document ID.
--- @param opts?      crap.FindByIdOptions  Optional options (e.g., `{ depth = 1 }`).
--- @return crap.Document?
function crap.collections.find_by_id(collection, id, opts) end

--- Create a new document.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string           Collection slug.
--- @param data       table<string, any> Field values.
--- @return crap.Document
function crap.collections.create(collection, data) end

--- Update an existing document.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string           Collection slug.
--- @param id         string           Document ID.
--- @param data       table<string, any> Fields to update (partial).
--- @return crap.Document
function crap.collections.update(collection, id, data) end

--- Delete a document.
--- Inside hooks, runs within the parent operation's transaction.
--- @param collection string  Collection slug.
--- @param id         string  Document ID.
--- @return boolean success
function crap.collections.delete(collection, id) end


-- ── crap.globals ─────────────────────────────────────────────

--- Global (singleton document) definition and runtime operations.
--- @class crap.globals
crap.globals = {}

--- Define a new global. Call this in global definition files.
--- @param slug   string            Unique global identifier.
--- @param config crap.GlobalConfig Global configuration.
function crap.globals.define(slug, config) end

--- Get a global's current value.
--- @param slug string  Global slug.
--- @return crap.Document
function crap.globals.get(slug) end

--- Update a global's value.
--- @param slug string              Global slug.
--- @param data table<string, any>  Fields to update.
--- @return crap.Document
function crap.globals.update(slug, data) end


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
--- | "after_login"
--- | "after_logout"
--- | "on_init"

--- Register a hook function for an event.
--- @param event crap.HookEvent  The lifecycle event to hook into.
--- @param fn    function         Hook function receiving context table.
function crap.hooks.register(event, fn) end

--- Remove a previously registered hook function.
--- @param event crap.HookEvent  The lifecycle event.
--- @param fn    function         The function to remove.
function crap.hooks.remove(event, fn) end


-- ── crap.log ─────────────────────────────────────────────────

--- Structured logging (maps to Rust tracing).
--- @class crap.log
crap.log = {}

--- Log an info-level message.
--- @param msg string  Log message.
--- @param data? table Additional structured data.
function crap.log.info(msg, data) end

--- Log a warning-level message.
--- @param msg string  Log message.
--- @param data? table Additional structured data.
function crap.log.warn(msg, data) end

--- Log an error-level message.
--- @param msg string  Log message.
--- @param data? table Additional structured data.
function crap.log.error(msg, data) end


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
