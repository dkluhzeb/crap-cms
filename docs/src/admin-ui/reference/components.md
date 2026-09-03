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
`_internal/util/discover.js::discoverSingleton(eventName)` helper
handles the dance:

```js
import { discoverSingleton } from './_internal/util/discover.js';

const drawer = discoverSingleton('crap:drawer-request');
drawer?.open({ title: 'Settings' });
```

### `window.crap` namespace (sugar)

`static/components/_internal/global.js` exposes a flat namespace as
the console-friendly / inline-template convenience layer:

```js
window.crap.toast({ message: 'Saved', type: 'success' });
window.crap.confirm('Delete this?').then((ok) => { … });
window.crap.drawer.open({ title: '…' });
window.crap.deleteDialog.open({ slug, id, title, softDelete });
window.crap.createPanel.open({ collection, title, onCreated });
window.crap.theme.set('tokyo-night');
window.crap.csrf();          // shorthand for _internal/util/cookies.js::readCsrfCookie
```

Both layers reach the same singleton instance — `window.crap` is sugar
over the canonical event-discovery + module APIs documented above.

## Singleton reference

<!-- GENERATED:components-singleton BEGIN -->
| Tag | Event | Stability | Summary | Source |
|---|---|---|---|---|
| `<crap-confirm-dialog>` | `crap:confirm-dialog-request` | stable | Standalone confirmation dialog for HTMX actions | `static/components/confirm-dialog.js` |
| `<crap-create-panel>` | `crap:create-panel-request` | stable | Inline Create Panel | `static/components/create-panel.js` |
| `<crap-delete-dialog>` | `crap:delete-dialog-request` | stable | Singleton delete confirmation dialog | `static/components/delete-dialog.js` |
| `<crap-drawer>` | `crap:drawer-request` | stable | Slide-in drawer panel | `static/components/drawer.js` |
| `<crap-session-dialog>` | — | stable | Session expiry warning | `static/components/session-guard.js` |
| `<crap-toast>` | `crap:toast-request` | stable | Toast notifications | `static/components/toast.js` |
<!-- GENERATED:components-singleton END -->

### `<crap-toast>` — `toast({ message, type?, duration? })`

```js
import { toast } from './_internal/util/toast.js';
toast({ message: 'Saved', type: 'success' });
// types: 'success' | 'error' | 'info' (default)
// duration: ms (default 3000; 0 = stays until dismissed)
```

Or via the event directly:

```js
document.dispatchEvent(new CustomEvent('crap:toast-request', {
  detail: { message: 'Hi', type: 'info' },
}));
```

### `<crap-drawer>` — `instance.open(opts)` / `instance.close()`

`opts`: `{ title }`. Opening clears the drawer body; the caller then
mounts its own content into the `instance.body` container (a plain
`HTMLDivElement` getter) after `open()` returns.

### `<crap-confirm-dialog>` — `instance.prompt(message, opts?)`

Returns `Promise<boolean>` — resolves `true` when the user clicks
Confirm, `false` on Cancel/Escape/backdrop. `<crap-confirm>` (the form-
intercepting variant) consumes this dialog automatically and falls back
to `window.confirm()` when no dialog is mounted.

### `<crap-delete-dialog>` — `instance.open({ slug, id, title, softDelete, canPermanentlyDelete? })`

Backs all `[data-delete-id]` buttons in the admin. After a successful
delete it toasts and navigates back to the collection list (no DOM
event is dispatched — list pages refresh via the navigation).

### `<crap-create-panel>` — `instance.open({ collection, title, onCreated })`

Inline create modal for relationship/upload fields. `onCreated(doc)` is
invoked with the new document on success.

## Form-field components

These wrap form-bound inputs. Their tags **must remain in light DOM**
(or a slot-projected light child) for the browser to submit the value.
Some also dispatch a bubbling `crap:change` event on edit so
`<crap-dirty-form>` can react (see the contract below).

<!-- GENERATED:components-form-field BEGIN -->
| Tag | Stability | Summary | Source |
|---|---|---|---|
| `<crap-array-field>` | experimental | Array and blocks field repeater | `static/components/array-fields.js` |
| `<crap-array-row>` | stable | Array/blocks row wrapper | `static/components/array-row.js` |
| `<crap-block-picker>` | stable | Block picker | `static/components/block-picker.js` |
| `<crap-code>` | stable | CodeMirror 6-based code editor | `static/components/code.js` |
| `<crap-conditions>` | stable | Display conditions | `static/components/conditions.js` |
| `<crap-confirm>` | stable | Confirmation guard around destructive form actions | `static/components/confirm.js` |
| `<crap-dirty-form>` | stable | Dirty Form Guard | `static/components/dirty-form.js` |
| `<crap-focal-point>` | stable | Focal point picker | `static/components/focal-point.js` |
| `<crap-password-toggle>` | stable | Password visibility toggle | `static/components/password-toggle.js` |
| `<crap-relationship-search>` | experimental | Relationship / upload field | `static/components/relationship-search.js` |
| `<crap-richtext>` | stable | ProseMirror-based WYSIWYG editor | `static/components/richtext.js` |
| `<crap-tags>` | stable | Tag input | `static/components/tags.js` |
| `<crap-upload-preview>` | stable | Upload-field preview | `static/components/uploads.js` |
| `<crap-validate-form>` | stable | Pre-submit validation for upload forms | `static/components/validate-form.js` |
<!-- GENERATED:components-form-field END -->

### `crap:change` event contract

`<crap-tags>` and `<crap-relationship-search>` dispatch a bubbling
`crap:change` event whenever the underlying form value changes;
`<crap-code>` dispatches one only when its **language picker** changes
(content edits reach the form as native `input` events from the
editor). No `detail` payload is guaranteed — tags and
relationship-search dispatch a plain `Event`, code a `CustomEvent`
with `{ name, value }` detail — so read the current value from the
host element / its hidden input. This is the canonical signal for
form-watchers (`<crap-dirty-form>` listens; `<crap-upload-preview>`
relays it internally).

## Page enhancers

Tag-only enhancements that auto-init on connect; no public API.

<!-- GENERATED:components-enhancer BEGIN -->
| Tag | Stability | Summary | Source |
|---|---|---|---|
| `<crap-back-refs>` | stable | Back-references lazy loader | `static/components/back-refs.js` |
| `<crap-collapsible>` | internal | Collapsible group/section | `static/components/_internal/groups.js` |
| `<crap-column-picker>` | stable | Column picker | `static/components/list-settings/column-picker.js` |
| `<crap-filter-builder>` | stable | Filter builder | `static/components/list-settings/filter-builder.js` |
| `<crap-list-settings>` | experimental | List settings | `static/components/list-settings.js` |
| `<crap-live-events>` | stable | Live event stream | `static/components/live-events.js` |
| `<crap-locale-picker>` | stable | Editor locale picker | `static/components/locale-picker.js` |
| `<crap-pill-list>` | stable | Pill / chip list | `static/components/pill-list.js` |
| `<crap-scroll-restore>` | stable | Form UI state preservation | `static/components/scroll.js` |
| `<crap-sidebar>` | stable | Mobile sidebar toggle | `static/components/sidebar-toggle.js` |
| `<crap-sticky-header>` | stable | Sticky page header | `static/components/sticky-header.js` |
| `<crap-tabs>` | stable | Tab field switching | `static/components/tabs.js` |
| `<crap-theme-picker>` | stable | Theme switcher | `static/components/theme.js` |
| `<crap-time>` | stable | Locale-aware date display | `static/components/time-format.js` |
| `<crap-ui-locale-picker>` | stable | Admin UI locale picker | `static/components/ui-locale-picker.js` |
<!-- GENERATED:components-enhancer END -->

### Three pickers via `CrapPickerBase`

`static/components/_internal/picker-base.js` is the shared toggle/
dropdown/outside-click base class for:

- `<crap-locale-picker>` — content-locale switcher (cookie-driven)
- `<crap-ui-locale-picker>` — admin-UI locale (server-persisted)
- `<crap-theme-picker>` — theme switcher (localStorage)

Subclasses declare static selectors (`toggleSelector`, `dropdownSelector`,
`itemSelector`, `openClass`, `valueDatasetKey`) and implement
`_onValue(value)`. About 25 LOC each.

## Util modules

Util modules live in the framework-reserved `_internal/` namespace.
Re-exported from `static/components/_internal/util/index.js`:

| Module                          | Exports                                                                    |
| ------------------------------- | -------------------------------------------------------------------------- |
| `_internal/util/cookies.js`     | `readCookie(name)`, `readCsrfCookie()`, `writeCookie(name, value, opts)`   |
| `_internal/util/toast.js`       | `toast({ message, type?, duration? })`                                     |
| `_internal/util/htmx.js`        | `getHttpVerb(event)` — normalise HTMX `htmx:configRequest` verb to upper   |
| `_internal/util/discover.js`    | `discoverSingleton(eventName)` — returns the discovered instance or `null` |
| `_internal/util/json.js`        | `parseJsonAttribute(el, attr, fallback)`, `readDataIsland(host, id, fallback)` |

## Internal helpers (not part of the public API)

- `static/components/_internal/h.js` — `h(tag, props, ...children)`
  typed DOM builder. Replaces `innerHTML` template strings.
- `static/components/_internal/css.js` — `` css`…` `` tagged template
  that returns a `CSSStyleSheet` for `adoptedStyleSheets`.
- `static/components/_internal/i18n.js` — `t(key)` reads the `crap-i18n` data
  island injected by `layout/base.hbs`. The island body is emitted by
  the server-side `{{{admin_i18n}}}` helper, which serialises a
  curated set of keys for the active `_locale` as a single JSON
  object. To expose extra keys to JS, edit the `ADMIN_JS_KEYS` list
  in `src/admin/templates/helpers/admin_i18n.rs`. Missing keys still
  resolve at the call site (`t()` falls back to the raw key) but
  show up untranslated.

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

The `_internal/` modules are technically overrideable file-by-file
too (`static/components/_internal/util/cookies.js` etc.), but the
underscore namespace is framework-reserved — no stability contract,
override at your own risk.

## Tooling contract

- **CSP**: `script-src` is nonce-based and `style-src` is `'self'`. No
  inline `style="…"` attributes; constructed stylesheets only via
  `css.js`. Components must not call `el.style.setProperty('--var', '…')`
  on light-DOM elements without an inline-style allowlist.
- **HTMX**: components don't fight HTMX swaps. They store cleanup state
  in `disconnectedCallback` so HTMX-replaced subtrees re-initialise
  cleanly on the next `connectedCallback`.
- **Tests**: `e2e/tests/browser_*.rs` exercise each component via
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
| `partials/htmx-nav-link.hbs`| `<a class="button" hx-get hx-target="#main" hx-push-url>` nav link (partial swap) |
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
| `layout/base.hbs`   | Authenticated admin chrome: head + sidebar + header + main slot. Contains the `{{#if htmx_partial}}` branch that serves partial navigations (`#main` swaps) — overrides must keep it |
| `layout/auth.hbs`   | Unauthenticated chrome: head + auth-card + slot for the form/content  |
| `layout/header.hbs` | Page header partial rendered by `base.hbs`                            |
| `layout/sidebar.hbs`| Left navigation rendered by `base.hbs`                                |

Pages use partial-block syntax: `{{#> layout/base}}…page content…{{/layout/base}}`.

See [CSS Variables](css-variables.md) for the design-token contract that
every component reads from.
