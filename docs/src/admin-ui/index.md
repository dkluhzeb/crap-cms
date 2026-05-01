# Admin UI

Crap CMS includes a built-in admin UI served via Axum with Handlebars
templates and HTMX. The admin is fully customizable through a
filesystem-overlay model — drop a same-named file into your config
directory to override any built-in template, stylesheet, or
component.

## How to customize

The customization surface has four axes — pick the one that matches
your goal:

| Goal | Layer | Where to start |
|---|---|---|
| Restyle (colors, spacing, fonts) | CSS tokens | [Themes guide](guides/themes.md), [CSS variables reference](reference/css-variables.md) |
| Tweak a page's HTML structure | Templates | [Template overlay guide](guides/template-overlay.md), [Template context reference](reference/template-context.md) |
| Replace or extend a component | Web Components | [Components reference](reference/components.md) |
| Add a custom content block (richtext node) | Lua | [Custom content block scenario](scenarios/06-custom-content-block.md) |
| Add a custom admin page | Lua + HBS template | [Custom admin page scenario](scenarios/05-custom-page.md) |
| Add a top-level custom field type (e.g. `rating`) | partial workarounds today; full support is a roadmap item | See ["What about a top-level field type?"](scenarios/06-custom-content-block.md#what-about-a-wholly-new-top-level-field-type) for shipped workarounds. |

The customization motion is always the same: **drop a file at the
matching path inside your config dir's `static/` or `templates/`
folder.** The admin overlay resolves config-dir paths first, compiled
defaults second.

### When to use what

There are six override mechanisms — pick by what kind of change you're
making, not by what file you happen to be looking at. Most one-off
customizations only need one of these.

| To… | Mechanism | Where |
|---|---|---|
| **Add** content at a defined extension point (extra dashboard widget, extra metadata tag, extra sidebar entry) | **slot** | drop `.hbs` files into `templates/slots/<name>/` |
| **Replace** a markup chunk reused in multiple places (logo, meta tags, icon-font link) | **partial** | drop a same-named file at `templates/partials/<name>.hbs` |
| **Replace** a single text value (site name) | **config field** | set `[admin] foo = "..."` in `crap.toml`, render via `{{crap.foo}}` |
| **Replace** a CSS rule or token (brand colors, spacing) | drop CSS | `static/styles/themes/themes-<name>.css` (or override `static/styles/parts/<name>.css`) |
| **Replace** an entire page template (collections list, dashboard) | drop HBS | drop a same-named file at `templates/<page>.hbs` |
| **Replace** a built-in Web Component (toast, drawer, …) | drop JS | drop a same-named file at `static/components/<name>.js` |
| **Add behavior** to a built-in component without replacing it (audit logging, analytics on toast) | capture-phase event listener | listen for the component's `crap:*` event from `static/components/custom.js` |

Quick rule of thumb:

- **Need new content?** Slot.
- **Editing a single value?** Config.
- **Editing markup?** Partial (if it's a chunk) or full template override (if it's a page).
- **Editing styling?** CSS file at the matching path.
- **Editing component code?** JS file at the matching path — or a capture-phase listener if you're just tacking behavior on.

The slot system is **additive** — your slot file runs *alongside*
upstream's defaults. Every other mechanism is **replacement** —
your file completely supersedes upstream's. Run `crap-cms templates
status` to see when an upstream replacement target has drifted from
the version you forked.

## Upgrading from a previous version?

The pre-1.0 layout reshuffle moved several files into role-grouped
subdirectories (`static/styles/`, `static/vendor/`, `static/icons/`,
`static/components/_internal/`). All old paths still resolve via
compatibility aliases for this release, with a deprecation warning
logged on first hit.

See **[Migrating from the old layout](upgrade/migrating-from-old-layout.md)**
for the full path map and an auto-generated migration recipe via
`crap-cms templates layout`.

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
- **Content-Security-Policy** header with configurable per-directive source lists (see `[admin.csp]`)
- CSRF protection on all forms and HTMX requests (double-submit cookie pattern)
- `Secure` flag on session cookies in production (`dev_mode = false`)
- Rate limiting on login (configurable max attempts and lockout duration)
- `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy`, `Permissions-Policy` headers

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

### Session Refresh Endpoint

`POST /admin/api/session-refresh` reissues the admin session cookie when the user is about to be logged out by token expiry.

- **Triggered by the client.** The `<crap-session-guard>` web component (`static/components/session-guard.js`) shows a pre-expiry warning toast and POSTs here when the operator clicks "Stay signed in". It is **not** polled on a fixed interval — only fired in response to the warning.
- **Authentication required.** The handler reads `Claims` from request extensions (populated by the admin auth middleware), so an unauthenticated request returns `401 Unauthorized` before any work is done. CSRF is enforced via the `X-CSRF-Token` header / `crap_csrf` cookie like the rest of the admin POST routes.
- **Re-validates the user before reissuing.** It checks that the user still exists, is not `_locked`, and that the token's `session_version` matches the current value in the auth collection (so a password change or session-version bump invalidates older tokens). Any failure returns `401`; a deleted user can't silently keep refreshing.
- **Behavior on success.** Issues a fresh JWT, sets `crap_session` (and the matching expiry cookie) with the same `SameSite`/`Secure` flags as login, and returns `204 No Content`. The body is empty — the client treats the new cookie as the success signal.
- **No explicit rate limit.** The endpoint is not separately rate-limited, but it requires a valid existing session (so it can't be abused unauthenticated) and the cost is one DB read plus a JWT sign.

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
- Web Components (Shadow DOM, CSS variables for theming) — see
  [Web Components](reference/components.md) for the full reference, including
  the singleton discovery contract, the `crap:change` form-field event,
  the `window.crap` namespace, and the override pattern. The library
  ships ~30 components: `<crap-toast>`, `<crap-confirm>`,
  `<crap-confirm-dialog>`, `<crap-richtext>`, `<crap-code>`,
  `<crap-tags>`, `<crap-array-field>`, `<crap-validate-form>`, …
