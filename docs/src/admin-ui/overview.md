# Admin UI

Crap CMS includes a built-in admin UI served via Axum with Handlebars templates and HTMX.

## Access

Default: [http://localhost:3000/admin](http://localhost:3000/admin)

When auth collections are configured, the admin UI requires login. Without auth collections, it's fully open.

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
| `/admin/globals/{slug}` | Global edit form |
| `/static/*` | Static assets (public) |
| `/uploads/*` | Uploaded files |

## CSS Architecture

- Custom properties in `:root` (colors, spacing, fonts, shadows)
- Separate files per concern: `layout.css`, `buttons.css`, `cards.css`, `forms.css`, `tables.css`
- Composed via `@import` in `styles.css`
- BEM-ish naming (`.block`, `.block__element`, `.block--modifier`)
- Geist font family (variable weight)

## JavaScript

- No build step, no npm, no bundler
- Single `components.js` file loaded with `<script defer>`
- JSDoc annotations for all types
- Web Components:
  - `<crap-toast>` — toast notifications (listens for `htmx:afterRequest`, reads `X-Crap-Toast` header)
  - `<crap-confirm>` — confirmation dialogs (wraps forms, intercepts submit)
