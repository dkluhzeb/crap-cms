# Routes

The admin UI exposes the routes below. The auth-flow routes (login,
logout, forgot/reset password, email verification, MFA, auth
callbacks) are reachable without a session; every other `/admin/`
route sits behind the auth middleware **when at least one auth
collection exists or `admin.require_auth` is set** (without either,
the admin runs unauthenticated). All state-changing routes are
CSRF-protected. `/static/` and `/uploads/` are public.

## HTML routes

| Route | Method | Description |
|---|---|---|
| `/` | GET | Dashboard (same handler as `/admin`) |
| `/admin` | GET | Dashboard |
| `/admin/login` | GET, POST | Login page / login action (public) |
| `/admin/logout` | POST | Logout |
| `/admin/mfa` | GET, POST | MFA challenge page / code verification (public auth flow) |
| `/admin/auth/callback/{name}` | GET, POST | External auth-method callback, single auth collection (public auth flow) |
| `/admin/auth/callback/{collection}/{name}` | GET, POST | External auth-method callback bound to a named auth collection (public auth flow) |
| `/admin/forgot-password` | GET, POST | Forgot password page / action (public) |
| `/admin/reset-password` | GET, POST | Reset password page / action (public, requires token) |
| `/admin/verify-email` | GET | Email verification (public, requires token) |
| `/admin/resend-verification` | GET, POST | Request a fresh verification link (public) |
| `/admin/collections` | GET | Collection list |
| `/admin/collections/{slug}` | GET, POST | Collection items list / create action |
| `/admin/collections/{slug}/create` | GET | Create form |
| `/admin/collections/{slug}/{id}` | GET, POST/PUT, DELETE | Edit form / update action / delete action |
| `/admin/collections/{slug}/{id}/delete` | GET | Delete confirmation |
| `/admin/collections/{slug}/{id}/undelete` | POST | Restore a soft-deleted item from trash |
| `/admin/collections/{slug}/empty-trash` | POST | Permanently delete everything in trash |
| `/admin/collections/{slug}/{id}/versions` | GET | Version history |
| `/admin/collections/{slug}/{id}/versions/{version_id}/restore` | GET, POST | Restore confirmation / restore action |
| `/admin/globals/{slug}` | GET, POST | Global edit form / update action |
| `/admin/globals/{slug}/versions` | GET | Global version history |
| `/admin/globals/{slug}/versions/{version_id}/restore` | GET, POST | Restore confirmation / restore action |

The collection items list validates its query parameters strictly: a
present-but-invalid `where[...]` filter (unknown operator or field,
system column, malformed key), an unknown/unsortable `sort` field, an
invalid `_status` value, or invalid pagination params returns **400
Bad Request** naming the offending parameter — invalid params are
never silently ignored.

Filter rows combine exactly as written. Every top-level `where[field][op]=value`
row must match, and so must every row inside one OR bucket — two rows on the
same field AND together (`where[tags][equals]=a&where[tags][equals]=b` asks
for a list holding both `a` and `b`, or on a single-value field for a value
that is both). "Any of" is an OR group: `where[or][G][N][field][op]=value`,
one bucket `N` per alternative. A has-many field is matched element by element
(see [Query & Filters](../../query-and-filters/overview.md#has-many-fields-element-by-element)).

A `_status` row inside an OR group (`where[or][G][N][_status][equals]=…`)
is accepted only when every row of that group filters `_status` (a status
union) or the group has a single bucket (a plain AND). Mixing `_status`
with another field across OR buckets returns **400**: the status filter
applies to the whole list, so honoring it inside an OR would silently turn
the OR into an AND. The 400 message is translated into the viewer's UI
language (translation key `filter_status_or_mixed`).

`_status` rows combine like every other row: top-level rows, and the rows of
one OR bucket, AND together — `where[_status][equals]=draft&where[_status][equals]=published`
lists no document, since none is both — while the buckets of an OR group are
a union. Contradicting rows keep one filter pill each.

The filter builder (`<crap-filter-builder>`) never produces that shape: a
row can join the row above it with OR only when both rows filter `_status`
or neither does. Otherwise its OR choice is disabled (an OR already picked
falls back to AND — also when a row's field is changed to or from
`_status`, or a row is added or removed) and the builder shows a hint
explaining why. `_status` rows can still be OR'd with each other.

The list only offers — as columns, sort headers, and filter fields — fields
the viewer may filter and sort on: never a `hidden` field, never one whose
`access.read` denies this viewer without row data, and (for filters) only
fields with a column on the collection's own table. A sort or filter the
viewer requests on such a field anyway returns **403** naming the field;
an `admin.default_sort` on a field the viewer cannot read is dropped for
that viewer instead of refusing the list.

Every list link — pages, cursors, sort headers, the search form, clearing
the search, removing a filter pill — keeps the view's `search`, `sort`,
`per_page`, filters, and trash view; links that change the filters or the
search reset the page position (`page`, `after_cursor`, `before_cursor`).

## API routes (admin)

| Route | Method | Description |
|---|---|---|
| `/admin/collections/{slug}/validate` | POST | Inline validation (create form) |
| `/admin/collections/{slug}/{id}/validate` | POST | Inline validation (edit form) |
| `/admin/collections/{slug}/evaluate-conditions` | POST | Display condition evaluation |
| `/admin/collections/{slug}/{id}/back-references` | GET | Lazy-loaded, access-filtered back-reference list for the delete dialog (`{ references, has_inaccessible }`) |
| `/admin/globals/{slug}/validate` | POST | Global inline validation |
| `/admin/globals/{slug}/evaluate-conditions` | POST | Global display condition evaluation |
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
| `/api/upload/{slug}` | File upload endpoint (POST; Bearer-token authenticated) |
| `/api/upload/{slug}/{id}` | Replace file (PATCH) / delete file (DELETE; Bearer-token authenticated) |
| `/mcp` | MCP HTTP endpoint (POST, when MCP is enabled) |

## Session refresh endpoint

`POST /admin/api/session-refresh` reissues the admin session cookie
when the user is about to be logged out by token expiry.

- **Triggered by the client.** The `<crap-session-dialog>` web
  component (`static/components/session-guard.js`) shows a
  pre-expiry warning toast and POSTs here when the operator clicks
  "Stay signed in". Not polled on a fixed interval — only fired in
  response to the warning.
- **Authentication required.** The handler reads `Claims` from
  request extensions (populated by the admin auth middleware), so
  an unauthenticated request returns `401 Unauthorized`. CSRF is
  enforced via the `X-CSRF-Token` header / `crap_csrf` cookie: a mutating
  request whose cookie is missing or empty is refused with 403 and the
  response carries a fresh cookie, so the next submit works without a
  reload; a URL-encoded body over 2 MiB answers 413; an `Authorization:
  Bearer` header skips the CSRF check only when it carries a non-empty
  token.
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
