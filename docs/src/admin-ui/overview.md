# Admin UI

Crap CMS includes a built-in admin UI served via Axum with Handlebars templates and HTMX.

## Access

Default: [http://localhost:3000/admin](http://localhost:3000/admin)

When auth collections are configured, the admin UI requires login. Without auth collections, it's fully open.

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
| `/admin` | Dashboard |
| `/admin/login` | Login page (public) |
| `/admin/logout` | Logout (public) |
| `/admin/collections/{slug}` | Collection list view |
| `/admin/collections/{slug}/create` | Create form |
| `/admin/collections/{slug}/{id}` | Edit form |
| `/admin/collections/{slug}/{id}/versions/{version_id}/restore` | Restore a previous version |
| `/admin/globals/{slug}` | Global edit form |
| `/static/*` | Static assets (public) |
| `/uploads/*` | Uploaded files |

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
