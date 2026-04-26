# Web Components

The admin UI is built from ~30 vanilla Web Components living in
`static/components/`. They fall into three roles:

- **Singletons** — one instance per page, mounted by the layout. Other
  code dispatches a request event to discover the instance and invoke
  methods on it (`<crap-toast>`, `<crap-drawer>`, `<crap-confirm-dialog>`,
  `<crap-delete-dialog>`, `<crap-create-panel>`).
- **Form fields** — wrap a slotted `<input>`/`<textarea>` and add
  behaviour (`<crap-tags>`, `<crap-code>`, `<crap-richtext>`,
  `<crap-password-toggle>`, `<crap-focal-point>`).
- **Page enhancers** — auto-init on connect to enhance the surrounding
  HTMX-rendered markup (`<crap-array-field>`, `<crap-sticky-header>`,
  `<crap-dirty-form>`, `<crap-validate-form>`, `<crap-conditions>`,
  `<crap-list-settings>`, `<crap-scroll-restore>`).

Every component is **registered automatically** by importing
`/static/components/index.js` (loaded by `layout/base.hbs` and
`layout/auth.hbs`). Override authors don't need to touch
`customElements.define()` directly.

## Discovery

### Singleton components: `crap:<name>-request` event

A component dispatches a CustomEvent with that name, then reads
`event.detail.instance` populated by the singleton's listener. The
`util/discover.js::discoverSingleton(eventName)` helper handles the
dance:

```js
import { discoverSingleton } from './util/discover.js';

const drawer = discoverSingleton('crap:drawer-request');
drawer?.open({ title: 'Settings', html: '<p>…</p>' });
```

### `window.crap` namespace (sugar)

`static/components/global.js` exposes a flat namespace as the
console-friendly / inline-template convenience layer:

```js
window.crap.toast({ message: 'Saved', type: 'success' });
window.crap.confirm('Delete this?').then((ok) => { … });
window.crap.drawer.open({ title: '…', html: '…' });
window.crap.deleteDialog.open({ slug, id, title });
window.crap.createPanel.open({ collection, onCreated });
window.crap.theme.set('tokyo-night');
window.crap.csrf();          // shorthand for util/cookies.js::readCsrfCookie
```

Both layers reach the same singleton instance — `window.crap` is sugar
over the canonical event-discovery + module APIs documented above.

## Singleton reference

| Tag                       | Role                       | Discovery event                    | Module                                    |
| ------------------------- | -------------------------- | ---------------------------------- | ----------------------------------------- |
| `<crap-toast>`            | Toast notifications        | `crap:toast-request`               | `static/components/toast.js`              |
| `<crap-drawer>`           | Right-side slide-in panel  | `crap:drawer-request`              | `static/components/drawer.js`             |
| `<crap-confirm-dialog>`   | Promise-based confirm      | `crap:confirm-dialog-request`      | `static/components/confirm-dialog.js`     |
| `<crap-delete-dialog>`    | Delete-document confirm    | `crap:delete-dialog-request`       | `static/components/delete-dialog.js`      |
| `<crap-create-panel>`     | Inline create-from-relation | `crap:create-panel-request`       | `static/components/create-panel.js`       |
| `<crap-session-dialog>`   | Idle-session warn / stay   | (mounted by `<crap-session-guard>`) | `static/components/session-guard.js`     |

### `<crap-toast>` — `toast({ message, type?, duration? })`

```js
import { toast } from './util/toast.js';
toast({ message: 'Saved', type: 'success' });
// types: 'success' | 'error' | 'info' (default)
// duration: ms (default 3500)
```

Or via the event directly:

```js
document.dispatchEvent(new CustomEvent('crap:toast-request', {
  detail: { message: 'Hi', type: 'info' },
}));
```

### `<crap-drawer>` — `instance.open(opts)` / `instance.close()`

`opts`: `{ title, html?, url? }`. If `url` is given the drawer fetches
the URL into its body via HTMX-style swap. `html` is a literal HTML
string (escape user input before passing).

### `<crap-confirm-dialog>` — `instance.prompt(message, opts?)`

Returns `Promise<boolean>` — resolves `true` when the user clicks
Confirm, `false` on Cancel/Escape/backdrop. `<crap-confirm>` (the form-
intercepting variant) consumes this dialog automatically and falls back
to `window.confirm()` when no dialog is mounted.

### `<crap-delete-dialog>` — `instance.open({ slug, id, title, soft, canPerm })`

Backs all `[data-delete-id]` buttons in the admin. Dispatches
`crap:document-deleted` after a successful delete so list pages can
refresh.

### `<crap-create-panel>` — `instance.open({ collection, onCreated, title? })`

Inline create modal for relationship/upload fields. `onCreated(doc)` is
invoked with the new document on success.

## Form-field components

These wrap form-bound inputs. Their tags **must remain in light DOM**
(or a slot-projected light child) for the browser to submit the value.
Most also dispatch `crap:change` (`{ name, value }`) on edit so
`<crap-validate-form>` and `<crap-conditions>` can react.

| Tag                          | Wraps                       | Notes                                                  |
| ---------------------------- | --------------------------- | ------------------------------------------------------ |
| `<crap-tags>`                | hidden `<input>`            | comma/Enter-separated tag input; emits `crap:change`   |
| `<crap-code>`                | hidden `<textarea>`         | CodeMirror 6 editor with theme-aware syntax highlight   |
| `<crap-richtext>`            | hidden `<textarea>`         | ProseMirror editor; HTML or JSON output                 |
| `<crap-password-toggle>`     | slotted `<input type=password>` | Shadow DOM; renders own toggle button + icon       |
| `<crap-focal-point>`         | slotted `<img>` + hidden inputs | Drag-to-set focal-point coordinates for image fields |
| `<crap-relationship-search>` | hidden `<input>`            | Search-and-pick references (single or has-many)         |
| `<crap-upload-preview>`      | hidden `<input>` + slot     | Drag-and-drop upload + preview                          |
| `<crap-block-picker>`        | hidden `<select>`           | Used inside blocks-field add-row UI                     |
| `<crap-array-field>`         | wraps an array-row container | Coordinates drag-reorder, add/remove, validation badge |
| `<crap-validate-form>`       | wraps a `<form>`            | Live server-side validation via `validate-url` attr     |
| `<crap-dirty-form>`          | wraps a `<form>`            | Warns on navigation away with unsaved edits             |
| `<crap-conditions>`          | wraps a `<form>`            | Show/hide fields based on `data-condition` JSON         |

### `crap:change` event contract

Every editable field component dispatches `new CustomEvent('crap:change',
{ detail: { name, value }, bubbles: true })` whenever the underlying
form value changes. This is the canonical signal for form-watchers
(`crap-conditions`, `crap-dirty-form`, `crap-validate-form`).

## Page enhancers

Tag-only enhancements that auto-init on connect; no public API.

| Tag                          | Role                                                       |
| ---------------------------- | ---------------------------------------------------------- |
| `<crap-sticky-header>`       | Sticky page-title bar with shadow on scroll                |
| `<crap-list-settings>`       | Column picker + filter builder for list pages              |
| `<crap-back-refs>`           | Lazy-loaded incoming-references panel                      |
| `<crap-collapsible>`         | Collapsible group/fieldset (uses `<details>` semantics)    |
| `<crap-tabs>`                | Tab switcher inside group fields                           |
| `<crap-sidebar>`             | Mobile sidebar toggle                                      |
| `<crap-scroll-restore>`      | Preserves scroll position across HTMX swaps                |
| `<crap-session-guard>`       | Idle-session warning + auto-extend                         |
| `<crap-live-events>`         | SSE subscription for live document updates                 |
| `<crap-time>`                | Locale-aware datetime formatter                            |

### Three pickers via `CrapPickerBase`

`static/components/picker-base.js` is the shared toggle/dropdown/
outside-click base class for:

- `<crap-locale-picker>` — content-locale switcher (cookie-driven)
- `<crap-ui-locale-picker>` — admin-UI locale (server-persisted)
- `<crap-theme-picker>` — theme switcher (localStorage)

Subclasses declare static selectors (`toggleSelector`, `dropdownSelector`,
`itemSelector`, `openClass`, `valueDatasetKey`) and implement
`_onValue(value)`. About 25 LOC each.

## Util modules

Re-exported from `static/components/util/index.js`:

| Module                | Exports                                                                    |
| --------------------- | -------------------------------------------------------------------------- |
| `util/cookies.js`     | `readCookie(name)`, `readCsrfCookie()`, `writeCookie(name, value, opts)`   |
| `util/toast.js`       | `toast({ message, type?, duration? })`                                     |
| `util/htmx.js`        | `getHttpVerb(event)` — normalise HTMX `htmx:configRequest` verb to upper   |
| `util/discover.js`    | `discoverSingleton(eventName)` — returns the discovered instance or `null` |
| `util/json.js`        | `parseJsonAttribute(el, attr, fallback)`, `readDataIsland(host, id, fallback)` |

## Internal helpers (not part of the public API)

- `static/components/h.js` — `h(tag, props, ...children)` typed DOM
  builder. Replaces `innerHTML` template strings.
- `static/components/css.js` — `` css`…` `` tagged template that
  returns a `CSSStyleSheet` for `adoptedStyleSheets`.
- `static/components/i18n.js` — `t(key)` reads the `crap-i18n` data
  island injected by `layout/base.hbs`.

## Override pattern

Every component lives under `static/components/<name>.js`. Drop a
replacement at the **same path** in your config directory's
`static/components/<name>.js` and it overrides the compiled default
(file-by-file overlay).

The override **must register the same custom-element tag** so existing
templates continue working. To extend instead of replace, import the
upstream class, subclass it, then re-define:

```js
// <config_dir>/static/components/toast.js
import { CrapToast as Base } from '/static/components/toast.js';

class CustomToast extends Base {
  show(opts) {
    console.log('toast intercepted', opts);
    return super.show(opts);
  }
}

customElements.define('crap-toast', CustomToast);
```

Util modules are individually overrideable too —
`static/components/util/cookies.js` etc.

## Tooling contract

- **CSP**: `script-src` is nonce-based and `style-src` is `'self'`. No
  inline `style="…"` attributes; constructed stylesheets only via
  `css.js`. Components must not call `el.style.setProperty('--var', '…')`
  on light-DOM elements without an inline-style allowlist.
- **HTMX**: components don't fight HTMX swaps. They store cleanup state
  in `disconnectedCallback` so HTMX-replaced subtrees re-initialise
  cleanly on the next `connectedCallback`.
- **Tests**: `tests/e2e/browser_*.rs` exercise each component via
  chromiumoxide. Add a regression test when changing public behaviour.

## Template partials

Server-side counterpart to the JS components. Lives in
`templates/partials/` and `templates/layout/`. Same overlay rules:
drop a file at the matching path in your config dir's `templates/`
folder to override.

### Partials (`templates/partials/`)

| Partial                     | Role                                                          |
| --------------------------- | ------------------------------------------------------------- |
| `partials/field.hbs`        | Wraps form input with label, required marker, locale badge, error, help; three variants: `default`, `fieldset` (radio groups), `checkbox` (slot-then-label) |
| `partials/sidebar-panel.hbs`| `<div class="edit-sidebar__panel">` with optional header (icon + label) and slotted body |
| `partials/array-row-header.hbs` | Drag handle + toggle + title slot + error badge + 4 action buttons; consumed by `<crap-array-field>` |
| `partials/htmx-nav-link.hbs`| `<a class="button" hx-get hx-target="body" hx-push-url>` link |
| `partials/status-badge.hbs` | `<span class="badge badge--{status}">{status}</span>`         |
| `partials/error-page.hbs`   | Full 404/403/500-style error card                             |
| `partials/warning-card.hbs` | `<div class="card card--warning">` with title and slotted body |
| `partials/loading-indicator.hbs` | HTMX `hx-indicator` target with `inline` and `sidebar` variants |
| `partials/form-actions.hbs` | `<div class="form__actions">` chrome + cancel link, action buttons via slot |
| `partials/breadcrumb.hbs`   | Crumbs trail                                                  |
| `partials/pagination.hbs`   | Prev / page-info / Next                                       |
| `partials/version-sidebar.hbs` | Version-history panel inside an edit sidebar               |
| `partials/version-table.hbs`| Full-width versions table                                     |

Most accept named parameters and inherit the rest of the call-site
context. Partials with body slots use the `{{#> partials/foo …}}…{{/}}`
block-call syntax and reference the slot via `{{> @partial-block }}`.

### Layouts (`templates/layout/`)

| Layout              | Role                                                                  |
| ------------------- | --------------------------------------------------------------------- |
| `layout/base.hbs`   | Authenticated admin chrome: head + sidebar + header + main slot       |
| `layout/auth.hbs`   | Unauthenticated chrome: head + auth-card + slot for the form/content  |
| `layout/header.hbs` | Page header partial rendered by `base.hbs`                            |
| `layout/sidebar.hbs`| Left navigation rendered by `base.hbs`                                |

Pages use partial-block syntax: `{{#> layout/base}}…page content…{{/layout/base}}`.

See [CSS Variables](css-variables.md) for the design-token contract that
every component reads from.
