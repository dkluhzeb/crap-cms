# Partials

Partials are **named, reusable chunks of HBS markup** included from
other templates via the `{{> name}}` syntax. They form one of the
admin UI's three customization mechanisms — use a partial when you
want to **replace a markup chunk** that's reused in multiple places
(logo, breadcrumb, status badge), without touching every template
that includes it.

```hbs
{{!-- A built-in template includes a partial --}}
<div class="header__logo">
  {{> partials/logo class="header__logo-icon"}}
  {{crap.site_name}}
</div>
```

```hbs
{{!-- You override the partial by dropping the same path in your config dir --}}
{{!-- <config_dir>/templates/partials/logo.hbs --}}
<svg class="{{class}}" viewBox="0 0 24 24">
  <path d="M ... your brand mark ..." />
</svg>
```

That's it — the override applies to every template that includes the
partial.

## How partial overrides work

Partials register under a name derived from their relative path in
`templates/`. So `templates/partials/logo.hbs` registers as
`partials/logo`. When the loader walks your config dir, files at the
same path register under the same name and **win** over the embedded
default.

`{{> partials/logo class="..."}}` then resolves to your file
everywhere it's invoked. No need to override the surrounding pages
(`header.hbs`, `auth.hbs`, etc.) — the include is the seam.

## Partial parameters

Partials take parameters via the `{{> name key=value}}` syntax. Inside
the partial, parameters are available at the root context:

```hbs
{{!-- caller --}}
{{> partials/htmx-nav-link href="/admin/users" label_key="users" variant="primary" icon="people"}}
```

```hbs
{{!-- partials/htmx-nav-link.hbs --}}
<a
  class="button button--{{variant}}"
  href="{{href}}"
  hx-get="{{href}}"
  hx-target="body"
  hx-push-url="true"
>
  {{#if icon}}<span class="material-symbols-outlined">{{icon}}</span>{{/if}}
  {{t label_key}}
</a>
```

Keep parameters narrow and well-named. The partial's parameter
contract is what overlay authors will code against — changing a
parameter name breaks every overlay that depends on it.

## Block-form partials

Partials can accept a body via the `{{#> name}}body content{{/name}}`
syntax. Useful when the partial is a **wrapper** around content the
caller provides:

```hbs
{{!-- caller --}}
{{#> partials/warning-card title_key="delete_has_references"}}
  <p>3 incoming references prevent deletion.</p>
  <a href="{{back_refs_url}}">View references</a>
{{/partials/warning-card}}
```

```hbs
{{!-- partials/warning-card.hbs --}}
<div class="card card--warning">
  <strong>{{t title_key}}</strong>
  {{> @partial-block}}      {{!-- caller's body renders here --}}
</div>
```

Block-form partials are how the admin's `partials/field.hbs` wrapper
adds the label/error/help chrome around every field-type's input
markup.

## Built-in partials

| Partial | Used for |
|---|---|
| `partials/logo.hbs` | Brand SVG. Caller passes `class`. |
| `partials/meta-tags.hbs` | `<head>` metadata: charset, viewport, theme-color, favicon link. |
| `partials/icon-font.hbs` | Material Symbols stylesheet `<link>`. Override for self-hosting / privacy. |
| `partials/field.hbs` | Block-form wrapper around field inputs (label, required indicator, error, help). |
| `partials/breadcrumb.hbs` | Page breadcrumb trail. |
| `partials/pagination.hbs` | List-view next/previous controls. |
| `partials/status-badge.hbs` | Document status pill (`published`, `draft`, etc.). Caller passes `status`. |
| `partials/loading-indicator.hbs` | HTMX request indicator. Caller passes `variant` (`inline` or `sidebar`). |
| `partials/array-row-header.hbs` | Drag-handle / move / duplicate / remove buttons for array-field rows. Caller passes `expanded`, `has_errors`. |
| `partials/error-page.hbs` | Error-page chrome (used by `errors/404.hbs`, etc.). |
| `partials/form-actions.hbs` | Save/Publish/Cancel button row at the bottom of edit forms. |
| `partials/htmx-nav-link.hbs` | Anchor with `hx-*` attributes for client-side nav. |
| `partials/sidebar-panel.hbs` | Collapsible sidebar panel wrapper (used by version history, back-refs). |
| `partials/version-sidebar.hbs` | Version-history sidebar contents. |
| `partials/version-table.hbs` | Version-history table on the standalone versions page. |
| `partials/warning-card.hbs` | Block-form `<div class="card card--warning">` wrapper. |

## Worked example — swap the brand logo

Goal: replace the heart-in-house mark with your company's wordmark.

**Step 1**: drop your SVG into the config dir at the partial's path:

```hbs
{{!-- <config_dir>/templates/partials/logo.hbs --}}
<svg
  class="{{class}}"
  xmlns="http://www.w3.org/2000/svg"
  viewBox="0 0 100 24"
  fill="currentColor"
>
  <text x="0" y="18" font-size="20" font-weight="700">ACME</text>
</svg>
```

**Step 2**: that's it. The wordmark renders in both the header
(`header.hbs` includes it via `{{> partials/logo class="header__logo-icon"}}`)
and the auth-card icon (`auth.hbs` includes it via
`{{> partials/logo class="auth-card__icon"}}`).

The `{{class}}` parameter is preserved so the SVG inherits the right
size / colour from the calling template's CSS.

## Worked example — self-host Material Symbols for privacy

Goal: don't load the icon font from Google Fonts (privacy / GDPR
compliance / air-gapped deployments).

**Step 1**: vendor the woff2 file (download from
<https://fonts.gstatic.com>) and a CSS file that points at it:

```
<config_dir>/static/vendor/material-symbols.css
<config_dir>/static/vendor/material-symbols.woff2
```

**Step 2**: override the `icon-font` partial to load your local copy:

```hbs
{{!-- <config_dir>/templates/partials/icon-font.hbs --}}
<link
  rel="stylesheet"
  href="/static/vendor/material-symbols.css"
/>
```

**Step 3**: that's it. Both layouts (`base.hbs` and `auth.hbs`) load
your local stylesheet instead of Google's CDN.

## When NOT to use a partial

- **Adding content** at a defined extension point — use a
  [slot](slots.md). Slots are additive; partials are replacement.
- **Replacing a single value** — use a config field. `{{crap.site_name}}`
  is a config field, not a partial.
- **Replacing a whole page template** — just drop the page file at
  `templates/<page>.hbs`. No partial wrapper needed.

Partials are specifically for **structured markup chunks reused in
multiple places**. Reaching for one when you really want additive
content (slot) or a single value (config) leads to over-coupling.
See the [admin UI overview](../index.md#when-to-use-what) for the
full mechanism matrix.

## Drift detection

Partials are part of the overlay drift surface. After overriding,
`crap-cms templates status` will show your file with a source-version
header (if extracted via `templates extract`) or a `no source header`
note (if hand-written). When upstream renames or restructures a
partial in a future release, `templates status` flags it so you can
re-port. See [Drift tooling](../upgrade/drift-tooling.md).
