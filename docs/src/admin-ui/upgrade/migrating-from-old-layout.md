# Migrating from the old overlay layout

In the run-up to 1.0, the admin UI customization surface was reshuffled
into a stable, role-grouped layout. This page is the **one-stop
migration guide** for any config dir built against the previous
layout.

> **Breaking change — no compatibility aliases.** Old paths now 404
> outright. Config-dir overlays at any old path silently stop serving
> after the upgrade. **Migrate before you upgrade**, using the recipe
> below.

## TL;DR

- **Public components** (`<crap-toast>`, `<crap-richtext>`, etc.):
  override paths are **unchanged**. If your config dir has
  `static/components/toast.js`, it still works exactly as before.
- **CSS files**: moved from `static/*.css` flat into
  `static/styles/{base,parts,layout,themes}/`. Overlays at the old
  flat paths return 404.
- **Vendored JS** (`htmx.js`, `codemirror.js`, `prosemirror.js`):
  moved to `static/vendor/`. Old paths 404.
- **Icons** (`favicon.svg`, `crap-cms.svg`): moved to `static/icons/`.
  Old paths 404.
- **Plumbing JS modules** (`h.js`, `css.js`, `i18n.js`, `util/`,
  others): moved to `static/components/_internal/`. Old paths 404.
  **Most users should never have overridden these.**
- **Templates**: not moved. Page-family folders (`auth/`,
  `collections/`, `dashboard/`, `errors/`, `globals/`) stay at the
  top level alongside `partials/`, `fields/`, `layout/`, `email/`.

Run `crap-cms templates layout` against your config dir for an
auto-generated migration recipe (`mkdir -p` + `git mv` lines plus
flagged manual cleanups). Run it **before** upgrading: once you're
on alpha.8, overlays at the old paths simply stop serving and the
admin falls back to the embedded defaults without a warning.

## Why the change

Five problems with the old layout, in roughly the order users hit
them:

1. **Visual clutter at the static root.** Fourteen sibling CSS files
   plus the entry stylesheet plus three vendor JS bundles plus two
   SVGs all rubbing shoulders. No directory hint to indicate which
   was which.
2. **No segregation between override targets and plumbing.** The
   `components/` folder mixed 33 public web components with 13
   internal helpers (`h.js`, `css.js`, `events.js`, `util/*`). New
   integrators couldn't tell which files were safe to fork.
3. **No "extend, don't replace" anchor.** Users wanting to add a thin
   subclass on top of a built-in component had no stable URL to import
   the upstream version from.
4. **No user seam for new components.** Adding a bespoke
   `<my-widget>` required a fork of `index.js` to get it imported.
5. **No upgrade visibility.** Once you'd customized files, there was
   no built-in tooling to tell you which ones had drifted from
   upstream or moved.

The reshuffle solves all five with the smallest blast radius
possible: public override paths stay unchanged, plumbing moves out of
the way, and three new conventions land alongside.

## What's new

### `static/styles/` — CSS organized into a system

```
static/styles/
  main.css                   # entry — @imports the rest in cascade order
  tokens.css                 # design tokens (CSS custom properties)
  base/
    normalize.css            # vendored normalize
    fonts.css                # @font-face declarations
    reset.css                # global resets, utilities, web-component plumbing
  parts/
    badges.css  breadcrumb.css  buttons.css  cards.css
    forms.css   lists.css   pagination.css   tables.css
  layout/
    layout.css  edit-sidebar.css
  themes/
    default.css              # light/dark default tokens
    # users drop themes-<name>.css here
```

The entry HTML reference is now `<link href="/static/styles/main.css">`.
The old URL `/static/styles.css` returns 404 — update overlays before
upgrading.

### `static/vendor/` — bundled third-party

`htmx.js`, `codemirror.js`, `prosemirror.js` moved here. The HBS
layout `<script>` tags now reference `/static/vendor/<name>.js`. Old
URLs (`/static/htmx.js`, etc.) return 404.

### `static/icons/` — SVGs

`favicon.svg` and `crap-cms.svg` moved here. The favicon `<link>` now
references `/static/icons/favicon.svg`. Old URLs return 404.

### `static/components/_internal/` — plumbing

The 13 internal helper modules moved into this reserved namespace:

```
static/components/_internal/
  css.js       global.js    groups.js
  h.js         i18n.js      picker-base.js
  util/
    cookies.js  discover.js  htmx.js
    index.js    json.js      toast.js
```

The leading underscore marks the namespace as **framework-reserved**.
Don't override these unless you know exactly what you're doing — they
have no stability contract. Public components are unaffected.

### Adding behavior without replacing — capture-phase event listeners

Most singletons (`<crap-toast>`, `<crap-drawer>`, `<crap-confirm-dialog>`,
etc.) are dispatch-discovered: code anywhere on the page fires a
`CustomEvent` and the singleton picks it up. To add behavior without
replacing the component, listen for the same event in the capture
phase and let it continue:

```js
// <config_dir>/static/components/custom.js
document.addEventListener('crap:toast-request', (e) => {
  fetch('/api/audit', {
    method: 'POST',
    body: JSON.stringify({ message: e.detail.message }),
  });
  // No stopPropagation() — upstream's <crap-toast> still gets the
  // event and shows the toast normally. Strictly additive.
}, true /* capture phase */);
```

This is **strictly additive** — you don't replace the toast component,
so upstream improvements to the toast UI flow through automatically.
Inheritance / subclassing isn't required for the common "I want to
add behavior X to component Y" goal.

For the rarer case where you genuinely need to replace the component
(e.g. swap out the entire rendering), drop a same-named file at
`<config_dir>/static/components/<name>.js`. Run
`crap-cms templates status` to see when upstream drifts so you can
re-port your changes.

### `static/components/custom.js` — bespoke component seam

If `<config_dir>/static/components/custom.js` exists, the admin's
`index.js` auto-imports it after all built-in atoms register. Use it
to register your own `<my-widget>` components without forking
`index.js`:

```js
// <config_dir>/static/components/custom.js
import './my-weather-card.js';
import './my-status-pill.js';
```

When the file is absent, the auto-import is a silent no-op.

## Path-by-path migration map

The full move table. Every old path in this table returns 404 in
alpha.8. Run `crap-cms templates layout` to get the exact `git mv`
lines for the files you actually have.

### Static — CSS

| Old path | New path |
|---|---|
| `static/styles.css` | `static/styles/main.css` |
| `static/normalize.css` | `static/styles/base/normalize.css` |
| `static/fonts.css` | `static/styles/base/fonts.css` |
| `static/badges.css` | `static/styles/parts/badges.css` |
| `static/breadcrumb.css` | `static/styles/parts/breadcrumb.css` |
| `static/buttons.css` | `static/styles/parts/buttons.css` |
| `static/cards.css` | `static/styles/parts/cards.css` |
| `static/forms.css` | `static/styles/parts/forms.css` |
| `static/tables.css` | `static/styles/parts/tables.css` |
| `static/layout.css` | `static/styles/layout/layout.css` |
| `static/edit-sidebar.css` | `static/styles/layout/edit-sidebar.css` |
| `static/themes.css` | `static/styles/themes/default.css` |
| `static/lists.css` *and* `static/list-toolbar.css` | merged → `static/styles/parts/lists.css` |
| (extracted from old `styles.css`) | `static/styles/tokens.css` |
| (extracted from old `styles.css`) | `static/styles/parts/pagination.css` |

### Static — vendor + icons

| Old path | New path |
|---|---|
| `static/htmx.js` | `static/vendor/htmx.js` |
| `static/codemirror.js` | `static/vendor/codemirror.js` |
| `static/prosemirror.js` | `static/vendor/prosemirror.js` |
| `static/favicon.svg` | `static/icons/favicon.svg` |
| `static/crap-cms.svg` | `static/icons/crap-cms.svg` |

### Static — plumbing JS

| Old path | New path |
|---|---|
| `static/components/css.js` | `static/components/_internal/css.js` |
| `static/components/global.js` | `static/components/_internal/global.js` |
| `static/components/groups.js` | `static/components/_internal/groups.js` |
| `static/components/h.js` | `static/components/_internal/h.js` |
| `static/components/i18n.js` | `static/components/_internal/i18n.js` |
| `static/components/picker-base.js` | `static/components/_internal/picker-base.js` |
| `static/components/util/cookies.js` | `static/components/_internal/util/cookies.js` |
| `static/components/util/discover.js` | `static/components/_internal/util/discover.js` |
| `static/components/util/htmx.js` | `static/components/_internal/util/htmx.js` |
| `static/components/util/index.js` | `static/components/_internal/util/index.js` |
| `static/components/util/json.js` | `static/components/_internal/util/json.js` |
| `static/components/util/toast.js` | `static/components/_internal/util/toast.js` |

### Public components — unchanged

The 33 public web components (`<crap-toast>`, `<crap-richtext>`,
`<crap-relationship-search>`, etc.) **stay at**
`static/components/<name>.js`. Override paths are unchanged.

### Templates — unchanged

No template files moved. `templates/auth/`, `templates/collections/`,
`templates/dashboard/`, etc. stay flat at the top level alongside
`templates/partials/`, `templates/fields/`, `templates/layout/`,
`templates/email/`.

## Auto-generated migration recipe

For any config dir built against the old layout:

```
$ crap-cms templates layout
```

The command walks your config dir, prints every old-layout file it
finds with its new-layout target, and emits copy-pasteable shell
commands (`mkdir -p`, `git mv`, plus `cat ... > ...` for the merged
`lists.css`/`list-toolbar.css` case). It also flags after-move
verifications the tool can't safely automate (`@import` paths inside
moved CSS, relative `import` paths inside moved JS).

The command is **read-only** — it describes; you transform.

## Why no compatibility aliases?

Every alias is a permanent maintenance tax: it pins a second public
URL to the same asset, has to be tested across upgrades, and tends to
outlive the deprecation it was meant to soften. The reshuffle is
small, mechanical, and tooling-assisted (`crap-cms templates layout`
gives you the exact `git mv` lines), so a one-shot migration is less
costly than maintaining aliases through 1.0 and beyond. If you need a
soft rollout, stage the upgrade in pre-prod with the migration
applied, verify under the strict CSP, then bump production.
