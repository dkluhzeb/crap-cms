# Admin UI

Crap CMS includes a built-in admin UI served via Axum with Handlebars templates and HTMX.

## Access

Default: [http://localhost:3000/admin](http://localhost:3000/admin)

Access to the admin panel is controlled by two gates:

1. **`require_auth`** (default: `true`) — when no auth collection exists, the admin shows a "Setup Required" page (HTTP 503) instead of being open. Set `require_auth = false` in `[admin]` for fully open dev mode.
2. **`access`** (optional Lua function ref) — checked after successful authentication. Gates which authenticated users can access the admin panel. Return `true` to allow, `false`/`nil` to show "Access Denied" (HTTP 403).

```toml
[admin]
require_auth = true                     # block admin if no auth collection (default)
access = "access.admin_panel"           # only allow users passing this function
```

```lua
-- access/admin_panel.lua
return function(ctx)
    return ctx.user and ctx.user.role == "admin"
end
```

When auth collections are configured and no `access` function is set, any authenticated user can access the admin.

**Security features:**
- CSRF protection on all forms and HTMX requests (double-submit cookie pattern)
- `Secure` flag on session cookies in production (`dev_mode = false`)
- Rate limiting on login (configurable max attempts and lockout duration)

## Technology

- **Axum** web framework for routing and middleware
- **Handlebars** templates with partial inheritance
- **HTMX** for dynamic page updates without JavaScript frameworks
- **Plain CSS** with custom properties (no preprocessor, no build step)
- **Web Components** with Shadow DOM (`<crap-toast>`, `<crap-confirm>`)

## Routes

| Route | Description |
|-------|-------------|
| `/health` | Liveness check (public) |
| `/ready` | Readiness check (public) |
| `/admin` | Dashboard |
| `/admin/login` | Login page (public) |
| `/admin/logout` | Logout (public) |
| `/admin/forgot-password` | Forgot password page (public) |
| `/admin/reset-password` | Reset password page (public) |
| `/admin/verify-email` | Email verification (public) |
| `/admin/collections` | Collection list |
| `/admin/collections/{slug}` | Collection items list |
| `/admin/collections/{slug}/create` | Create form |
| `/admin/collections/{slug}/{id}` | Edit form |
| `/admin/collections/{slug}/{id}/delete` | Delete confirmation |
| `/admin/collections/{slug}/{id}/versions` | Version history |
| `/admin/collections/{slug}/{id}/versions/{version_id}/restore` | Restore a version |
| `/admin/collections/{slug}/validate` | Inline validation (POST) |
| `/admin/collections/{slug}/evaluate-conditions` | Display condition evaluation (POST) |
| `/admin/globals/{slug}` | Global edit form |
| `/admin/globals/{slug}/validate` | Global inline validation (POST) |
| `/admin/globals/{slug}/versions` | Global version history |
| `/admin/globals/{slug}/versions/{version_id}/restore` | Restore global version |
| `/admin/events` | SSE live update stream |
| `/admin/api/search/{slug}` | Relationship search endpoint |
| `/admin/api/session-refresh` | Session token refresh (POST) |
| `/admin/api/locale` | Save locale preference (POST) |
| `/admin/api/user-settings/{slug}` | Save user settings (POST) |
| `/api/upload/{slug}` | File upload endpoint (POST) |
| `/mcp` | MCP HTTP endpoint (POST, if enabled) |
| `/static/*` | Static assets (public) |
| `/uploads/{collection_slug}/{filename}` | Uploaded files |

## Versioning & Drafts Workflow

For collections with `versions = { drafts = true }`, the admin UI provides a draft/publish workflow:

**List view:**
- Shows all documents (both draft and published) with status badges
- A "Status" column displays `published` or `draft` per row

**Create form:**
- **Publish** (primary button) — creates as published, enforces required field validation
- **Save as Draft** (secondary button) — creates as draft, skips required field validation

**Edit form:**
- **Draft document:** "Publish" (primary) + "Save Draft" (secondary) buttons
- **Published document:** "Update" (primary) + "Save Draft" (secondary) + "Unpublish" (ghost) buttons
- Draft saves create a version snapshot only — the main (published) document is not modified until you publish

**Sidebar:**
- Status badge showing current document status
- Version history panel listing recent versions with version number, status, date, and a "Restore" button
- Restoring a version writes the snapshot data back to the main table and creates a new version entry

Collections without `versions` configured work exactly as before — a single "Create" or "Update" button with no status management.

See [Versions & Drafts](../collections/versions.md) for the full configuration and behavioral reference.

## CSS Architecture

- Custom properties in `:root` (colors, spacing, fonts, shadows)
- Separate files per concern: `layout.css`, `buttons.css`, `cards.css`, `forms.css`, `tables.css`
- Composed via `@import` in `styles.css`
- BEM-ish naming (`.block`, `.block__element`, `.block--modifier`)
- Geist font family (variable weight)

## JavaScript

- No build step, no npm, no bundler — browser-native ES modules
- `static/components/index.js` entry point loaded with `<script type="module">`
- Each feature is a separate module under `static/components/` (individually overridable)
- JSDoc annotations for all types
- Web Components (Shadow DOM, CSS variables for theming):
  - `<crap-toast>` — toast notifications (listens for `htmx:afterRequest`, reads `X-Crap-Toast` header)
  - `<crap-confirm>` — confirmation dialogs (wraps forms, intercepts submit)
  - `<crap-confirm-dialog>` — standalone confirm for `hx-confirm` attributes (replaces native `window.confirm`)
  - `<crap-richtext>` — ProseMirror WYSIWYG editor
