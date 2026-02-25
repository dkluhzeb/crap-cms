# Template Context API

Every admin page receives a structured context object built by the `ContextBuilder`. When [overriding templates](template-overlay.md), you can access any of these variables in your Handlebars templates.

## Top-Level Keys

| Key | Type | Pages | Description |
|-----|------|-------|-------------|
| `crap` | object | all | App metadata (version, dev mode, auth status) |
| `page` | object | all | Current page info (title, type, breadcrumbs) |
| `nav` | object | all (except auth) | Navigation data for sidebar |
| `user` | object | authenticated | Current user (email, id, collection) |
| `collection` | object | collection pages | Full collection definition with metadata |
| `global` | object | global pages | Full global definition with metadata |
| `document` | object | edit pages | Current document with raw data |
| `fields` | array | edit/create/global edit | Processed field contexts for form rendering |
| `items` | array | collection items | Document list with enriched data |
| `editing` | boolean | edit/create | `true` when editing, `false` when creating |
| `pagination` | object | items, versions | Pagination state |
| `versions` | array | edit (versioned) | Recent version entries |
| `has_more_versions` | boolean | edit (versioned) | Whether more versions exist beyond the shown 3 |
| `upload` | object | upload collections | Upload file metadata and preview |
| `collection_cards` | array | dashboard | Collection summary cards with counts |
| `global_cards` | array | dashboard | Global summary cards |
| `search` | string | items | Current search query |
| `custom` | object | all | Custom data injected by `before_render` hooks |

---

## Base Context (Every Page)

### `crap` — App Metadata

```handlebars
{{crap.version}}      {{!-- "0.1.0" --}}
{{crap.dev_mode}}     {{!-- true/false --}}
{{crap.auth_enabled}} {{!-- true if any auth collection exists --}}
```

### `page` — Page Info

```handlebars
{{page.title}}  {{!-- "Edit Post", "Dashboard", etc. --}}
{{page.type}}   {{!-- "collection_edit", "dashboard", etc. --}}

{{#each page.breadcrumbs}}
  {{#if this.url}}
    <a href="{{this.url}}">{{this.label}}</a>
  {{else}}
    <span>{{this.label}}</span>
  {{/if}}
{{/each}}
```

#### Page Types

| Type | Route |
|------|-------|
| `dashboard` | `/admin` |
| `collection_list` | `/admin/collections` |
| `collection_items` | `/admin/collections/{slug}` |
| `collection_edit` | `/admin/collections/{slug}/{id}` |
| `collection_create` | `/admin/collections/{slug}/create` |
| `collection_delete` | `/admin/collections/{slug}/{id}/delete` |
| `collection_versions` | `/admin/collections/{slug}/{id}/versions` |
| `global_edit` | `/admin/globals/{slug}` |
| `auth_login` | `/admin/login` |
| `auth_forgot` | `/admin/forgot-password` |
| `auth_reset` | `/admin/reset-password` |
| `error_403` | (forbidden) |
| `error_404` | (not found) |
| `error_500` | (server error) |

### `nav` — Navigation

Available on all authenticated pages. Auth pages use `ContextBuilder::auth()` which does **not** include nav.

```handlebars
{{#each nav.collections}}
  <a href="/admin/collections/{{this.slug}}">{{this.display_name}}</a>
  {{!-- Also available: this.is_auth, this.is_upload --}}
{{/each}}

{{#each nav.globals}}
  <a href="/admin/globals/{{this.slug}}">{{this.display_name}}</a>
{{/each}}
```

Each nav collection entry:

| Field | Type | Description |
|-------|------|-------------|
| `slug` | string | Collection slug |
| `display_name` | string | Human-readable name |
| `is_auth` | boolean | Whether this is an auth collection |
| `is_upload` | boolean | Whether this is an upload collection |

Each nav global entry:

| Field | Type | Description |
|-------|------|-------------|
| `slug` | string | Global slug |
| `display_name` | string | Human-readable name |

### `user` — Current User

Present when the user is authenticated (JWT session valid). Absent on auth pages and error pages.

```handlebars
{{#if user}}
  Logged in as {{user.email}} ({{user.collection}})
{{/if}}
```

| Field | Type | Description |
|-------|------|-------------|
| `email` | string | User's email address |
| `id` | string | User document ID |
| `collection` | string | Auth collection slug (e.g., `"users"`) |

---

## Collection Pages

### `collection` — Collection Definition

Available on all collection page types (`collection_items`, `collection_edit`, `collection_create`, `collection_delete`, `collection_versions`).

```handlebars
{{collection.slug}}
{{collection.display_name}}
{{collection.singular_name}}
{{collection.title_field}}
{{collection.timestamps}}       {{!-- boolean --}}
{{collection.is_auth}}          {{!-- boolean --}}
{{collection.is_upload}}        {{!-- boolean --}}
{{collection.has_drafts}}       {{!-- boolean --}}
{{collection.has_versions}}     {{!-- boolean --}}
```

#### `collection.admin`

```handlebars
{{collection.admin.use_as_title}}          {{!-- field name or null --}}
{{collection.admin.default_sort}}          {{!-- e.g., "-created_at" or null --}}
{{collection.admin.hidden}}                {{!-- boolean --}}
{{collection.admin.list_searchable_fields}} {{!-- array of field names --}}
```

#### `collection.upload`

`null` unless the collection has upload enabled.

```handlebars
{{#if collection.upload}}
  Accepts: {{collection.upload.mime_types}}
  Max size: {{collection.upload.max_file_size}}
  Thumbnail: {{collection.upload.admin_thumbnail}}
{{/if}}
```

#### `collection.versions`

`null` unless the collection has versioning enabled.

```handlebars
{{#if collection.versions}}
  Drafts: {{collection.versions.drafts}}
  Max versions: {{collection.versions.max_versions}}
{{/if}}
```

#### `collection.auth`

`null` unless the collection is an auth collection.

```handlebars
{{#if collection.auth}}
  Local login: {{#if (not collection.auth.disable_local)}}enabled{{/if}}
  Email verification: {{collection.auth.verify_email}}
{{/if}}
```

#### `collection.fields_meta`

Array of raw field definitions. Useful for JavaScript conditional logic or embedding metadata.

```handlebars
{{#each collection.fields_meta}}
  {{this.name}} — {{this.field_type}}
  Required: {{this.required}}, Localized: {{this.localized}}
  Label: {{this.admin.label}}
{{/each}}

{{!-- Serialize to JSON for JavaScript --}}
<script>
  const fields = {{{json collection.fields_meta}}};
</script>
```

Each entry in `fields_meta`:

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Field name |
| `field_type` | string | `text`, `number`, `select`, `relationship`, etc. |
| `required` | boolean | Whether the field is required |
| `unique` | boolean | Whether the field has a unique constraint |
| `localized` | boolean | Whether the field is localized |
| `admin.label` | string/null | Display label |
| `admin.hidden` | boolean | Whether the field is hidden in admin |
| `admin.readonly` | boolean | Whether the field is readonly |
| `admin.width` | number/null | Column width hint |
| `admin.description` | string/null | Help text |
| `admin.placeholder` | string/null | Placeholder text |

---

## Dashboard

**Page type:** `dashboard`

Additional keys:

| Key | Type | Description |
|-----|------|-------------|
| `collection_cards` | array | One entry per collection with document count |
| `global_cards` | array | One entry per global |

```handlebars
{{#each collection_cards}}
  <a href="/admin/collections/{{this.slug}}">
    {{this.display_name}} ({{this.count}} items)
  </a>
{{/each}}

{{#each global_cards}}
  <a href="/admin/globals/{{this.slug}}">{{this.display_name}}</a>
{{/each}}
```

Collection card fields: `slug`, `display_name`, `singular_name`, `count`.

Global card fields: `slug`, `display_name`.

---

## Collection Items (List)

**Page type:** `collection_items`

Additional keys beyond `collection`:

| Key | Type | Description |
|-----|------|-------------|
| `items` | array | Document list |
| `search` | string/null | Current search query |
| `pagination` | object | Pagination state |
| `has_drafts` | boolean | Shorthand for `collection.has_drafts` |

### `items`

```handlebars
{{#each items}}
  <tr>
    <td>{{this.title_value}}</td>
    <td>{{this.status}}</td>
    <td>{{this.created_at}}</td>
    <td>{{this.updated_at}}</td>
    {{#if this.thumbnail_url}}
      <td><img src="{{this.thumbnail_url}}" /></td>
    {{/if}}
  </tr>
{{/each}}
```

Each item:

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Document ID |
| `title_value` | string | Value of the title field (falls back to filename for uploads, then ID) |
| `created_at` | string/null | Creation timestamp |
| `updated_at` | string/null | Last update timestamp |
| `status` | string | `"published"` or `"draft"` |
| `thumbnail_url` | string/null | Thumbnail URL (upload collections with images only) |

### `pagination`

```handlebars
{{#if pagination.has_prev}}
  <a href="{{pagination.prev_url}}">Previous</a>
{{/if}}
<span>Page {{pagination.page}} of {{pagination.total_pages}}</span>
{{#if pagination.has_next}}
  <a href="{{pagination.next_url}}">Next</a>
{{/if}}
```

| Field | Type | Description |
|-------|------|-------------|
| `page` | integer | Current page number (1-based) |
| `per_page` | integer | Items per page |
| `total` | integer | Total document count |
| `total_pages` | integer | Total number of pages |
| `has_prev` | boolean | Whether a previous page exists |
| `has_next` | boolean | Whether a next page exists |
| `prev_url` | string | URL for the previous page |
| `next_url` | string | URL for the next page |

---

## Collection Edit

**Page type:** `collection_edit`

Additional keys beyond `collection`:

| Key | Type | Description |
|-----|------|-------------|
| `document` | object | Current document data |
| `fields` | array | Processed field contexts for form rendering |
| `editing` | boolean | Always `true` |
| `has_drafts` | boolean | Shorthand for `collection.has_drafts` |
| `has_versions` | boolean | Shorthand for `collection.has_versions` |
| `versions` | array | Up to 3 most recent version entries |
| `has_more_versions` | boolean | `true` if more than 3 versions exist |
| `upload` | object | Upload context (upload collections only) |

### `document`

```handlebars
{{document.id}}
{{document.created_at}}
{{document.updated_at}}
{{document.status}}        {{!-- "published" or "draft" --}}

{{!-- Raw field values --}}
{{document.data.title}}
{{document.data.category}}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Document ID |
| `created_at` | string/null | Creation timestamp |
| `updated_at` | string/null | Last update timestamp |
| `status` | string | `"published"` or `"draft"` (when drafts enabled) |
| `data` | object | Raw field values as key-value pairs |

> **Draft loading:** When a collection has drafts enabled and the latest version is a draft, the edit page loads the document from the draft version snapshot. This means `document.data` contains the draft values, including block and array data — not the published main-table values.

### `fields`

Processed field context objects used by the `{{render_field}}` helper or custom form rendering. See [Field Context](#field-context) below.

### `versions`

```handlebars
{{#each versions}}
  v{{this.version}} — {{this.status}}
  {{#if this.latest}} (latest) {{/if}}
  Created: {{this.created_at}}
{{/each}}
{{#if has_more_versions}}
  <a href="/admin/collections/{{collection.slug}}/{{document.id}}/versions">View all</a>
{{/if}}
```

Each version entry: `id`, `version` (number), `status` (`"published"` or `"draft"`), `latest` (boolean), `created_at`.

### `upload`

Present only on upload collection edit/create pages.

```handlebars
{{#if collection.is_upload}}
  {{#if upload.preview}}
    <img src="{{upload.preview}}" />
  {{/if}}
  {{#if upload.info}}
    {{upload.info.filename}}
    {{upload.info.filesize_display}}  {{!-- e.g., "2.4 MB" --}}
    {{upload.info.dimensions}}        {{!-- e.g., "1920x1080" --}}
  {{/if}}
  {{#if upload.accept}}
    <input type="file" accept="{{upload.accept}}" />
  {{/if}}
{{/if}}
```

| Field | Type | Description |
|-------|------|-------------|
| `accept` | string/null | MIME type filter for file input (e.g., `"image/*"`) |
| `preview` | string/null | Preview image URL (images only, uses admin_thumbnail) |
| `info` | object/null | File info for existing uploads |
| `info.filename` | string | Original filename |
| `info.filesize_display` | string | Human-readable file size |
| `info.dimensions` | string/null | Image dimensions (e.g., `"1920x1080"`) |

---

## Collection Create

**Page type:** `collection_create`

Same structure as collection edit, with these differences:
- `editing` is `false`
- `document` is absent
- `versions`, `has_more_versions` are absent
- Password field is added for auth collections (required)

---

## Collection Delete

**Page type:** `collection_delete`

| Key | Type | Description |
|-----|------|-------------|
| `collection` | object | Collection definition |
| `document_id` | string | ID of the document to delete |
| `title_value` | string/null | Display title of the document |

---

## Collection Versions

**Page type:** `collection_versions`

Full version history page with pagination.

| Key | Type | Description |
|-----|------|-------------|
| `collection` | object | Collection definition |
| `document` | object | Stub with `id` only |
| `doc_title` | string | Document title for breadcrumbs |
| `versions` | array | Paginated version entries |
| `pagination` | object | Pagination state |

---

## Collection List

**Page type:** `collection_list`

| Key | Type | Description |
|-----|------|-------------|
| `collections` | array | All registered collections |

Each entry: `slug`, `display_name`, `field_count`.

---

## Global Edit

**Page type:** `global_edit`

| Key | Type | Description |
|-----|------|-------------|
| `global` | object | Global definition |
| `fields` | array | Processed field contexts |

### `global`

```handlebars
{{global.slug}}
{{global.display_name}}
{{#each global.fields_meta}}
  {{this.name}} — {{this.field_type}}
{{/each}}
```

| Field | Type | Description |
|-------|------|-------------|
| `slug` | string | Global slug |
| `display_name` | string | Human-readable name |
| `fields_meta` | array | Same structure as `collection.fields_meta` |

---

## Auth Pages

Auth pages use a minimal context builder (`ContextBuilder::auth()`) — no `nav` or `user`.

### Login (`auth_login`)

| Key | Type | Description |
|-----|------|-------------|
| `collections` | array | Auth collections (slug + display_name) |
| `show_collection_picker` | boolean | `true` if more than one auth collection |
| `disable_local` | boolean | `true` if all auth collections disable local login |
| `show_forgot_password` | boolean | `true` if email is configured and any collection enables forgot password |
| `error` | string/null | Error message (e.g., "Invalid email or password") |
| `success` | string/null | Success message (e.g., after password reset) |
| `email` | string/null | Pre-filled email (on error re-render) |

### Forgot Password (`auth_forgot`)

| Key | Type | Description |
|-----|------|-------------|
| `collections` | array | Auth collections |
| `show_collection_picker` | boolean | `true` if more than one auth collection |
| `success` | boolean/null | `true` after form submission (always, to avoid leaking user existence) |

### Reset Password (`auth_reset`)

| Key | Type | Description |
|-----|------|-------------|
| `token` | string/null | Reset token (if valid) |
| `error` | string/null | Error message (invalid/expired token, validation errors) |

---

## Error Pages

Error pages receive the base context (`crap`, `nav`, `page`) plus:

| Key | Type | Description |
|-----|------|-------------|
| `message` | string | Error description |

**Page types:** `error_403`, `error_404`, `error_500`.

```handlebars
<h1>{{page.title}}</h1>
<p>{{message}}</p>
```

---

## Locale Context

When localization is enabled in `crap.toml`, edit/create pages receive additional keys merged into the top level:

| Key | Type | Description |
|-----|------|-------------|
| `has_locales` | boolean | Always `true` when locale is enabled |
| `current_locale` | string | Currently selected locale (e.g., `"en"`) |
| `locales` | array | All configured locales with selection state |

```handlebars
{{#if has_locales}}
  {{#each locales}}
    <option value="{{this.value}}" {{#if this.selected}}selected{{/if}}>
      {{this.label}}
    </option>
  {{/each}}
{{/if}}
```

Each locale entry: `value` (e.g., `"en"`), `label` (e.g., `"EN"`), `selected` (boolean).

When editing a non-default locale, non-localized fields are rendered as readonly (locale-locked).

---

## Field Context

The `fields` array contains processed field context objects, one per field. These are used by `{{{render_field field}}}` or can be iterated manually.

### Common Fields

Every field context object has:

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Field name (HTML form input name) |
| `field_type` | string | Type identifier (e.g., `"text"`, `"select"`, `"blocks"`) |
| `label` | string | Display label (from `admin.label` or auto-generated) |
| `required` | boolean | Whether the field is required |
| `value` | string | Current value (stringified) |
| `placeholder` | string/null | Placeholder text |
| `description` | string/null | Help text |
| `readonly` | boolean | Whether the field is readonly |
| `localized` | boolean | Whether the field is localized |
| `locale_locked` | boolean | `true` when editing a non-default locale and the field is not localized |
| `error` | string/null | Validation error message (on re-render after failed save) |

### Select Fields

Additional keys:

| Field | Type | Description |
|-------|------|-------------|
| `options` | array | Available options |

Each option: `label`, `value`, `selected` (boolean).

### Checkbox Fields

| Field | Type | Description |
|-------|------|-------------|
| `checked` | boolean | Whether the checkbox is checked |

### Date Fields

| Field | Type | Description |
|-------|------|-------------|
| `picker_appearance` | string | `"dayOnly"`, `"dayAndTime"`, `"timeOnly"`, or `"monthOnly"` |
| `date_only_value` | string | Date portion only, e.g., `"2026-01-15"` (dayOnly) |
| `datetime_local_value` | string | Date+time, e.g., `"2026-01-15T09:00"` (dayAndTime) |

### Relationship Fields

| Field | Type | Description |
|-------|------|-------------|
| `relationship_collection` | string | Related collection slug |
| `has_many` | boolean | Whether this is a has-many relationship |
| `relationship_options` | array | Available documents from the related collection |

Each option: `value` (document ID), `label` (title field value), `selected` (boolean).

### Upload Fields

| Field | Type | Description |
|-------|------|-------------|
| `relationship_collection` | string | Upload collection slug |
| `relationship_options` | array | Available uploads with thumbnail info |
| `selected_preview_url` | string/null | Preview URL of the currently selected upload |
| `selected_filename` | string/null | Filename of the currently selected upload |

Each option: `value`, `label`, `selected`, `thumbnail_url` (if image), `is_image` (boolean), `filename`.

### Group Fields

| Field | Type | Description |
|-------|------|-------------|
| `sub_fields` | array | Sub-field contexts (same structure as top-level fields) |
| `collapsed` | boolean | Whether the group starts collapsed |

Sub-field `name` is formatted as `group__subfield` (double underscore).

### Array Fields

| Field | Type | Description |
|-------|------|-------------|
| `sub_fields` | array | Sub-field definitions (template for new rows) |
| `rows` | array | Existing row data |
| `row_count` | integer | Number of existing rows |

Each row: `index` (integer), `sub_fields` (array of field contexts with indexed names like `items[0][title]`).

### Blocks Fields

| Field | Type | Description |
|-------|------|-------------|
| `block_definitions` | array | Available block types with their fields |
| `rows` | array | Existing block instances |
| `row_count` | integer | Number of existing blocks |

Each block definition: `block_type`, `label`, `fields` (array of sub-field contexts).

Each row: `index`, `_block_type`, `block_label`, `sub_fields` (array of field contexts with indexed names like `content[0][heading]`).

---

## Handlebars Helpers

In addition to the standard Handlebars helpers, these custom helpers are available:

### Logic Helpers

| Helper | Usage | Description |
|--------|-------|-------------|
| `eq` | `{{#if (eq a b)}}` | Equality check (any types) |
| `not` | `{{#if (not val)}}` | Boolean negation |
| `and` | `{{#if (and a b)}}` | Logical AND |
| `or` | `{{#if (or a b)}}` | Logical OR |

### Comparison Helpers

| Helper | Usage | Description |
|--------|-------|-------------|
| `gt` | `{{#if (gt a b)}}` | Greater than (numeric) |
| `lt` | `{{#if (lt a b)}}` | Less than (numeric) |
| `gte` | `{{#if (gte a b)}}` | Greater than or equal (numeric) |
| `lte` | `{{#if (lte a b)}}` | Less than or equal (numeric) |
| `contains` | `{{#if (contains haystack needle)}}` | Array/string contains |

### Utility Helpers

| Helper | Usage | Description |
|--------|-------|-------------|
| `json` | `{{{json value}}}` | Serialize to JSON string (use triple braces) |
| `default` | `{{default val "fallback"}}` | Fallback for falsy values |
| `concat` | `{{concat a b c}}` | String concatenation (variadic) |
| `t` | `{{t "key"}}` | Translation lookup |
| `render_field` | `{{{render_field field}}}` | Render a field partial (use triple braces) |

### Translation Helper

Supports interpolation:

```handlebars
{{t "welcome"}}                     {{!-- simple lookup --}}
{{t "greeting" name="World"}}       {{!-- replaces {{name}} in translation string --}}
```

### Truthiness

Helpers like `not`, `and`, `or`, and `default` use Handlebars truthiness:
- **Falsy:** `null`, `false`, `0`, `""` (empty string), `[]` (empty array)
- **Truthy:** everything else (including empty objects `{}`)

### Composition

Helpers can be composed as sub-expressions:

```handlebars
{{#if (and (not collection.is_auth) (gt pagination.total 0))}}
  Showing {{pagination.total}} items
{{/if}}

{{#if (or collection.has_drafts collection.has_versions)}}
  This collection supports versioning
{{/if}}

<a href="{{concat "/admin/collections/" collection.slug "/create"}}">New</a>
```

---

## `before_render` Hook

You can inject custom data into every admin page context using the `before_render` hook:

```lua
-- init.lua
crap.hooks.register("before_render", function(context)
  context.custom = {
    announcement = "Maintenance tonight at 10pm",
    feature_flags = { new_editor = true },
  }
  return context
end)
```

Then in your templates:

```handlebars
{{#if custom.announcement}}
  <div class="announcement">{{custom.announcement}}</div>
{{/if}}

{{#if custom.feature_flags.new_editor}}
  {{!-- show new editor --}}
{{/if}}
```

The `before_render` hook:
- Fires on every admin page render (GET and POST error re-renders)
- Receives the full template context as a Lua table
- Must return the (possibly modified) context
- Has **no CRUD access** (no database operations) — keeps it fast
- Use the `custom` key by convention for injected data
- Can read and modify any context key (not just `custom`), but modifying built-in keys may break default templates
- On error: logs a warning and returns the original context unmodified

### Example: Conditional Navigation

```lua
crap.hooks.register("before_render", function(context)
  -- Add environment indicator
  local env = crap.env.get("APP_ENV") or "development"
  context.custom = context.custom or {}
  context.custom.environment = env
  context.custom.is_production = (env == "production")
  return context
end)
```

```handlebars
{{#if custom.is_production}}
  <div class="env-badge env-badge--production">PRODUCTION</div>
{{else}}
  <div class="env-badge">{{custom.environment}}</div>
{{/if}}
```

---

## Full Example: Custom List Template

A complete example overriding the items list to add a custom column:

```handlebars
{{!-- <config_dir>/templates/collections/items.hbs --}}
{{#> layout/base}}

<h1>{{page.title}}</h1>

{{#if custom.announcement}}
  <div class="alert">{{custom.announcement}}</div>
{{/if}}

<table>
  <thead>
    <tr>
      <th>Title</th>
      <th>Status</th>
      {{#if collection.is_upload}}<th>Preview</th>{{/if}}
      <th>Updated</th>
    </tr>
  </thead>
  <tbody>
    {{#each items}}
      <tr>
        <td>
          <a href="/admin/collections/{{../collection.slug}}/{{this.id}}">
            {{this.title_value}}
          </a>
        </td>
        <td>{{this.status}}</td>
        {{#if ../collection.is_upload}}
          <td>
            {{#if this.thumbnail_url}}
              <img src="{{this.thumbnail_url}}" width="40" />
            {{/if}}
          </td>
        {{/if}}
        <td>{{this.updated_at}}</td>
      </tr>
    {{/each}}
  </tbody>
</table>

{{#if pagination.has_prev}}
  <a href="{{pagination.prev_url}}">Previous</a>
{{/if}}
{{#if pagination.has_next}}
  <a href="{{pagination.next_url}}">Next</a>
{{/if}}

{{!-- Embed field metadata for client-side use --}}
<script>
  const collectionDef = {{{json collection}}};
</script>

{{/layout/base}}
```
