# Scenario 7: Add a custom field type

**Goal**: add a "rating" field to a collection — a 1–5 integer
backed by a custom star-picker UI in the admin, while keeping the
data sortable, filterable, and stored as a real `INTEGER` column.

**Difficulty**: medium. ~20 minutes from scratch using the scaffold;
maybe an hour by hand. Three coordinated files.

**You'll touch**: a Lua plugin file, an HBS template, and a Web
Component — or run `crap-cms make field <name>` to generate all
three at once.

## Approach

Crap CMS lets any field instance opt out of its type's default
render template by declaring `admin.template = "fields/<name>"` on
the field, plus optional per-field config via `admin.extra`. The
field stays storage-typed as the underlying built-in (`number`,
`text`, `select`, …) — SQL schema, validation, sorting, filtering
all flow through the built-in type — but the admin form renders a
custom widget you control.

The pattern is three coordinated files:

1. **A Lua plugin** that wraps the built-in field type with the
   per-field `admin.template` and `admin.extra` baked in, so
   collection schemas just call `rating.field({ name = "..." })`.
2. **A per-field HBS template** at
   `<config_dir>/templates/fields/<name>.hbs` that renders your
   widget inside the standard `partials/field` chrome.
3. **A Web Component** at
   `<config_dir>/static/components/<tag>.js` that drives the actual
   UI (clickable stars, color picker, drag handle, etc.).

## Step 1 — scaffold (recommended)

Run the `make field` command:

```
$ crap-cms make field rating --base-type number
+ Created /path/to/config/templates/fields/rating.hbs
+ Created /path/to/config/plugins/rating.lua
+ Created /path/to/config/static/components/crap-rating.js
+ Created field 'rating' — three files wired together via admin.template.

Use it in a collection:

  local rating = require("plugins.rating")

  crap.collections.define("products", {
    fields = {
      rating.field({ name = "my_rating" }),
      ...
    },
  })
```

This generates skeletons of all three files, pre-wired so they work
together. The Web Component is a form-associated custom element —
its value participates in native form submission and validation
without any hidden-input shim.

## Step 2 — register the Web Component

The scaffold prints a one-line import to add to your `custom.js`:

```js
// <config_dir>/static/components/custom.js
import './crap-rating.js';
```

`custom.js` is auto-imported from `index.js` on every admin page, so
your component registers once at boot.

## Step 3 — use it in a collection

```lua
-- <config_dir>/init.lua
local rating = require("plugins.rating")

crap.collections.define("products", {
  fields = {
    crap.fields.text({ name = "name", required = true }),
    rating.field({
      name = "rating",
      required = true,
      admin = {
        extra = { color = "amber" },  -- override per-instance
      },
    }),
  },
})
```

`rating.field({ ... })` is just `crap.fields.number({ min=1, max=5,
admin.template="fields/rating", admin.extra={...}, ... })` with
sensible defaults — the wrapper is a few lines of Lua you can edit
freely.

## Step 4 — flesh out the Web Component

The scaffold writes a placeholder render. Replace the body of
`_render()` with your real UI. For a star picker:

```js
// <config_dir>/static/components/crap-rating.js
class CrapRating extends HTMLElement {
  static formAssociated = true;
  static observedAttributes = ['value'];

  constructor() {
    super();
    this._internals = this.attachInternals();
    this.attachShadow({ mode: 'open' });
  }

  connectedCallback() {
    this._render();
  }

  attributeChangedCallback() {
    this._internals.setFormValue(this.value || '');
    this._render();
  }

  get value() { return this.getAttribute('value') ?? ''; }
  set value(v) { this.setAttribute('value', String(v)); }

  _render() {
    const max = Number(this.dataset.max ?? 5);
    const current = Number(this.value || 0);
    const color = this.dataset.color || 'amber';
    this.shadowRoot.innerHTML = `
      <style>
        :host { display: inline-flex; gap: 0.125rem; }
        button { background: none; border: none; cursor: pointer;
                 font-size: 1.5rem; color: var(--text-tertiary); }
        button[data-on] { color: var(--color-${color}, gold); }
      </style>
      ${Array.from({ length: max }, (_, i) => {
        const filled = i < current;
        return `<button type="button" data-i="${i + 1}" ${filled ? 'data-on' : ''}>★</button>`;
      }).join('')}
    `;
    for (const btn of this.shadowRoot.querySelectorAll('button')) {
      btn.addEventListener('click', () => {
        this.value = btn.dataset.i;
        this.dispatchEvent(new Event('crap:change', { bubbles: true, composed: true }));
      });
    }
  }
}

if (!customElements.get('crap-rating')) {
  customElements.define('crap-rating', CrapRating);
}
```

`crap:change` is the [public event](../reference/events.md) that
`<crap-dirty-form>` listens for to mark the form as having unsaved
changes — fire it on every value change.

## Step 5 — restart and verify

Lua loads at startup. Restart crap-cms (or rely on dev-mode
template reload for the HBS).

Open `/admin/collections/products/create`. The `rating` field
renders as five clickable stars; clicking one updates the form
value; the standard label/required/error chrome wraps the widget.

`crap-cms templates status` reports your three new files as
`· user-original`:

```
$ crap-cms templates status
  · plugins/rating.lua                       —  user-original (no upstream counterpart)
  · static/components/crap-rating.js         —  user-original (no upstream counterpart)
  · templates/fields/rating.hbs              —  user-original (no upstream counterpart)
```

## How this stays robust

- **Per-instance template binding.** `admin.template = "fields/rating"`
  opts *only* this field out of `fields/<type>` lookup in
  `RenderFieldHelper`. Every other `number` field keeps using the
  built-in `fields/number.hbs`. No global template override, no
  field-name matching.
- **`admin.extra` is freeform per-field config.** Pass `color`,
  `icon`, `max_stars`, anything JSON-serializable — your template
  reads it as `{{extra.<key>}}`. Same template + component reused
  across fields with different settings without forking.
- **Storage stays correct.** A rating field is still a `number`;
  the SQL column is `INTEGER`; `min`/`max` validation works;
  sorting and filtering use the integer value natively. Only the
  admin rendering is custom.
- **Path safety.** `admin.template` paths are validated at
  field-parse time — only `[a-zA-Z0-9/_-]` allowed, no `..`, no
  absolute paths, no empty segments. A bad path is rejected at
  startup with a clear error.
- **Deeply nested fields work.** The same per-field binding flows
  through array rows, group sub-fields, and tab panels —
  enrichment preserves `template` and `extra` regardless of
  nesting depth (regression-tested).

## What this scenario *doesn't* cover

A rating field built this way is still a `number` underneath. If
you specifically need:

- A new SQL column type distinct from `INTEGER` / `TEXT` / etc.
  (e.g. a custom enum stored with bespoke encoding),
- Custom validation rules at the field-type level (not workable as
  a `before_change` hook on the collection),
- The field type to ship as part of a published collection schema
  that other crap-cms instances must accept,

…then you need a top-level `FieldType` variant — a Rust change.
Lua-registered top-level field types are a tracked roadmap item
beyond the per-field-template mechanism above.

For 95% of "I want a custom widget for this kind of value" goals
(ratings, color pickers, slug builders, spinners, audio-trim
controls, …) the three-file pattern above is enough. The shipped
example has the rating field working — see
[`example/plugins/rating.lua`](https://github.com/dkluhs/crap-cms/blob/main/example/plugins/rating.lua),
[`example/templates/fields/rating.hbs`](https://github.com/dkluhs/crap-cms/blob/main/example/templates/fields/rating.hbs),
and [`example/static/components/crap-stars.js`](https://github.com/dkluhs/crap-cms/blob/main/example/static/components/crap-stars.js).

## When to use richtext nodes instead

If your custom data shape lives **inside content** (an article body
with embedded CTAs, mentions, callouts) rather than as a top-level
column on the collection, see [Scenario 6: Add a custom richtext
node](06-custom-richtext-node.md). Richtext nodes share the typed-
attrs + Web-Component pattern but are scoped to richtext field
content rather than row-level columns.
