# Routes

The admin UI exposes the routes below. All under `/admin/` are
authenticated and CSRF-protected (where applicable). `/static/` and
`/uploads/` are public.

## HTML routes

| Route | Description |
|---|---|
| `/admin` | Dashboard |
| `/admin/login` | Login page (public) |
| `/admin/logout` | Logout |
| `/admin/forgot-password` | Forgot password page (public) |
| `/admin/reset-password` | Reset password page (public, requires token) |
| `/admin/verify-email` | Email verification (public, requires token) |
| `/admin/collections` | Collection list |
| `/admin/collections/{slug}` | Collection items list |
| `/admin/collections/{slug}/create` | Create form |
| `/admin/collections/{slug}/{id}` | Edit form |
| `/admin/collections/{slug}/{id}/delete` | Delete confirmation |
| `/admin/collections/{slug}/{id}/versions` | Version history |
| `/admin/collections/{slug}/{id}/versions/{version_id}/restore` | Restore a version (POST) |
| `/admin/globals/{slug}` | Global edit form |
| `/admin/globals/{slug}/versions` | Global version history |
| `/admin/globals/{slug}/versions/{version_id}/restore` | Restore global version (POST) |

## API routes (admin)

| Route | Method | Description |
|---|---|---|
| `/admin/collections/{slug}/validate` | POST | Inline validation |
| `/admin/collections/{slug}/evaluate-conditions` | POST | Display condition evaluation |
| `/admin/globals/{slug}/validate` | POST | Global inline validation |
| `/admin/events` | GET | SSE live update stream |
| `/admin/api/search/{slug}` | GET | Relationship search |
| `/admin/api/session-refresh` | POST | Reissue session cookie before expiry |
| `/admin/api/locale` | POST | Save user's locale preference |
| `/admin/api/user-settings/{slug}` | POST | Save list-view column / filter selections |

## Public routes

| Route | Description |
|---|---|
| `/health` | Liveness check |
| `/ready` | Readiness check |
| `/static/*` | Static assets (overlay-served — see [Static files guide](../guides/static-files.md)) |
| `/uploads/{collection_slug}/{filename}` | Uploaded files |
| `/api/upload/{slug}` | File upload endpoint (POST) |
| `/mcp` | MCP HTTP endpoint (POST, when MCP is enabled) |

## Session refresh endpoint

`POST /admin/api/session-refresh` reissues the admin session cookie
when the user is about to be logged out by token expiry.

- **Triggered by the client.** The `<crap-session-guard>` web
  component (`static/components/session-guard.js`) shows a
  pre-expiry warning toast and POSTs here when the operator clicks
  "Stay signed in". Not polled on a fixed interval — only fired in
  response to the warning.
- **Authentication required.** The handler reads `Claims` from
  request extensions (populated by the admin auth middleware), so
  an unauthenticated request returns `401 Unauthorized`. CSRF is
  enforced via the `X-CSRF-Token` header / `crap_csrf` cookie.
- **Re-validates the user before reissuing.** Checks that the user
  still exists, is not `_locked`, and that the token's
  `session_version` matches the current value in the auth
  collection (so a password change or session-version bump
  invalidates older tokens). Any failure returns `401`; a deleted
  user can't silently keep refreshing.
- **Behavior on success.** Issues a fresh JWT, sets `crap_session`
  (and the matching expiry cookie) with the same `SameSite` /
  `Secure` flags as login, and returns `204 No Content`. The body
  is empty — the client treats the new cookie as the success
  signal.
- **No explicit rate limit.** The endpoint requires a valid existing
  session (so it can't be abused unauthenticated) and the cost is
  one DB read plus a JWT sign.

## Custom admin pages

A subset of admin URLs are **filesystem-routed**: any HBS template
at `<config_dir>/templates/pages/<slug>.hbs` is automatically served
at `/admin/p/<slug>` (slug-validated, case-sensitive). No Rust code
or fork required.

| Route | Description |
|---|---|
| `/admin/p/{slug}` | Renders `<config_dir>/templates/pages/{slug}.hbs` against the standard admin context. Sidebar entry, label, icon, and per-page access control come from `crap.pages.register("{slug}", { ... })` in `init.lua`. |

Slugs are restricted to `a-z`, `0-9`, `-`, `_`. Pages without a
`crap.pages.register` block route normally but don't appear in the
sidebar nav.

See [Scenario 5: Add a custom admin page](../scenarios/05-custom-page.md)
for the full walkthrough including the worked
`example/templates/pages/system_info.hbs` reference.

## Adding routes that aren't filesystem-routed

The fixed admin route table (everything above the custom-page
section) is in `src/admin/server.rs`. Adding routes outside the
`/admin/p/{slug}` pattern requires a fork — for example, a
`/admin/api/my-endpoint` POST handler. For most admin extensibility
needs, the custom-page mechanism + the existing API endpoints
(SSE, search, validate, user-settings) cover the use cases without
a fork.
