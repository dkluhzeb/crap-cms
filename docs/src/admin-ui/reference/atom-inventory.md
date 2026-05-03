# Atom Inventory

> **Spec doc, not yet enforced.** This page enumerates every web
> component in `static/components/` and proposes what its stable public
> surface should be — the version we'd commit to *not* break. It's the
> reference for the eventual Phase 3 refactor (atomization, versioned
> APIs, narrow stable surface). Today's components mostly already obey
> the proposed contracts; the discrepancies are flagged.

## Method

For each component we record:

- **Tag** — the `<crap-*>` element name.
- **Role** — `atom` (single concern, well-bounded), `composition`
  (multi-piece, internally orchestrates other elements; candidates for
  breakdown), or `singleton` (one instance per page, accessed via
  request-event discovery).
- **Stable surface** — the public API that overlay authors and custom
  widget authors program against.
  - **Attributes** — host-element attributes that affect behaviour.
  - **Events** — fired or listened to. References the named exports in
    [`static/components/events.js`](../../../static/components/events.js).
  - **Slots / parts** — for shadow-DOM atoms only.
- **Internal** — anything explicitly *not* part of the contract.
  Inner DOM structure, internal classes, internal event names — all
  free to change.

Atoms < 200 LOC are listed first within each category.

## Singletons

One instance per page, mounted by the layout. Discoverable via the
events in [`events.js`](../../../static/components/events.js):

| Tag                     | Discovery event              | Methods (stable)                                              | LOC  |
| ----------------------- | ---------------------------- | ------------------------------------------------------------- | ---- |
| `<crap-toast>`          | `EV_TOAST_REQUEST`           | `show({ message, type, duration })`                           | 172  |
| `<crap-drawer>`         | `EV_DRAWER_REQUEST`          | `open({ title })` → mount into `.body`; `close()`             | 379  |
| `<crap-confirm-dialog>` | `EV_CONFIRM_DIALOG_REQUEST`  | `prompt(message, opts) → Promise<boolean>`                    | 183  |
| `<crap-delete-dialog>`  | `EV_DELETE_DIALOG_REQUEST`   | `open({ slug, id, title })`                                   | 377  |
| `<crap-create-panel>`   | `EV_CREATE_PANEL_REQUEST`    | `open({ collection, onCreated })`; `close()`                  | 433  |
| `<crap-session-dialog>` | (auto-mounted, no API)       | —                                                             | 352  |

**Stable contract per singleton**:
- The discovery event name (constant in `events.js`).
- The methods listed above; their argument shapes.
- The `detail.instance` discovery-write protocol.

**Internal**: shadow DOM contents, CSS, internal state, internal class
names. The current implementations all use Shadow DOM with adopted
stylesheets — overlay authors restyle via CSS custom properties (which
pierce shadow boundaries) or `::part(...)` selectors where exposed.

## Atoms (form-shaped)

Wrap a slotted form input and add behaviour. Form-associated where
appropriate; emit `EV_CHANGE` (bubbling) when value changes so
`<crap-dirty-form>` can mark the form dirty.

| Tag                         | Slot                       | Public attributes                       | Events fired       | LOC  |
| --------------------------- | -------------------------- | --------------------------------------- | ------------------ | ---- |
| `<crap-password-toggle>`    | `<input type="password">`  | —                                       | —                  | 104  |
| `<crap-tags>`               | hidden `<input>`           | `data-min-length`, `data-max-length`, `data-min`, `data-max`, `data-placeholder`, `data-readonly`, `data-error`, `data-field-type` | `EV_CHANGE`        | 286  |
| `<crap-code>`               | hidden `<input>` + `<textarea>` | `data-language`, `data-languages`, `data-readonly` | `EV_CHANGE`        | 408  |
| `<crap-focal-point>`        | `<img>` + hidden `<input>` | —                                       | —                  | 142  |
| `<crap-richtext>`           | hidden `<input>` + `<textarea>` | `data-features`, `data-format`, `data-nodes` | `EV_CHANGE`        | 487  |

**Stable contract**:
- The host attributes listed.
- `EV_CHANGE` fires (bubbling) on value change.
- The slotted input remains light-DOM and form-participating
  (browser submits it natively; htmx serializes it normally).

**Internal**: shadow DOM, internal CSS classes, internal event names.

## Atoms (page-enhancement)

Light-DOM components that decorate server-rendered markup or are
mounted dynamically by an orchestrator. Some inject their own
stylesheet via `document.adoptedStyleSheets` on first connect (noted).

| Tag                          | Role                                                              | LOC  |
| ---------------------------- | ----------------------------------------------------------------- | ---- |
| `<crap-confirm>`             | Wrap a child form, prompt before submit                            | 89   |
| `<crap-dirty-form>`          | Mark form dirty on `EV_CHANGE`, prompt on navigate-away             | 187  |
| `<crap-sticky-header>`       | Pin a header on scroll                                             | 50   |
| `<crap-sidebar>`             | Sidebar toggle behaviour                                           | 83   |
| `<crap-time>`                | Format ISO timestamps in user's locale                             | 57   |
| `<crap-collapsible>`         | Group/collapsible toggle                                           | 51   |
| `<crap-locale-picker>`       | Editor locale switcher                                             | 35   |
| `<crap-ui-locale-picker>`    | Admin UI locale switcher                                           | 49   |
| `<crap-theme-picker>`        | Theme switcher (light/dark/auto + tokyo-night etc.)                | 95   |
| `<crap-tabs>`                | Tab keyboard nav + URL-hash sync                                   | 107  |
| `<crap-block-picker>`        | Wraps a `<select>` of block types; emits `EV_REQUEST_ADD_BLOCK`     | 222  |
| `<crap-uploads>`             | Drag-drop upload handler                                           | 128  |
| `<crap-back-refs>`           | Lazy-loaded back-references panel                                  | 134  |
| `<crap-conditions>`          | Display-conditions evaluator                                       | 276  |
| `<crap-scroll-restore>`      | Scroll position preservation across htmx swaps                     | 241  |
| `<crap-validate-form>`       | Native HTML5 form-validation surfacing                             | 451  |
| `<crap-live-events>`         | SSE subscriber for live document updates                           | 315  |
| `<crap-array-row>`           | Array/blocks row wrapper; owns row label-watcher logic             | 105  |
| `<crap-pill-list>`           | Chip cluster; `data-items` JSON; emits `crap:pill-removed`. *Self-styled* | 197  |
| `<crap-column-picker>`       | Drawer body for picking visible columns; htmx-submits             | 189  |
| `<crap-filter-builder>`      | Drawer body for `where[…]` filter composition. Exports `OPS_BY_TYPE` | 593  |

**Stable contract** (page-enhancement common):
- The host-element tag itself (presence on the page = behaviour active).
- Documented `data-*` attributes.
- Events listened to (the contract with surrounding components).

**Internal**: how the component reads/writes the DOM; CSS classes it
adds; htmx-event piggybacking.

## Compositions — refactor candidates

These render and orchestrate multiple internal pieces. They're the
primary atomization targets for Phase 3.

### `<crap-array-field>` — array + blocks repeater (545 LOC orchestrator + 105 LOC atom)

Status: **partially atomized (Phase 3.2).** Extracted `<crap-array-row>`
(stable atom, 105 LOC) which owns the row label-watcher concern. The
remaining orchestrator handles add/remove/reorder/duplicate via event
delegation, drag-and-drop sorting, index rewriting on the cloned
template subtree, and max-rows enforcement.

A further split into `<crap-array-controls>` (the +/-/duplicate/move
buttons) was scoped *out* of Phase 3.2 — the buttons today are inline
`<button data-action="...">` rendered by Handlebars and dispatched
via the orchestrator's click delegation. Splitting them would require
rebuilding the action wiring as inter-component events; there's no
demonstrated demand for swapping just the buttons today, so we
stopped at the row.

### `<crap-list-settings>` — list-page toolbar (atomized in 3.3)

Status: **fully atomized.** The toolbar's two drawer bodies are
standalone web components mounted by the orchestrator:

| Tag                          | LOC | Stable surface                                                                              |
| ---------------------------- | --- | ------------------------------------------------------------------------------------------- |
| `<crap-list-settings>`       | 163 | Orchestrator: `data-action="open-column-picker"` / `data-action="open-filter-builder"` buttons; htmx-search focus preservation. |
| `<crap-column-picker>`       | 189 | `data-collection` (slug) + `data-options` (JSON `ColumnOption[]`); fires `crap:column-picker-saved` on htmx success. |
| `<crap-filter-builder>`      | 593 | `data-collection` (slug) + `data-fields` (JSON `FieldMeta[]`); fires `crap:filter-builder-applied` on apply. Exports `OPS_BY_TYPE` for subclass extension. |

**Orchestration**: the toolbar listens for `data-action` clicks,
discovers the page's `<crap-drawer>` singleton via `EV_DRAWER_REQUEST`,
and mounts a freshly constructed picker/builder element (with data
already attached on its dataset) into the drawer body. The element's
`connectedCallback` builds the UI; the orchestrator listens for the
element's completion event to close the drawer.

**Override patterns** for users:
- **Full replace**: drop a replacement at
  `<config_dir>/static/components/list-settings/{column-picker,filter-builder}.js`.
- **Subclass**: import `CrapColumnPicker` / `CrapFilterBuilder` from
  the modules and extend; redefine the tag in your override file.
  Both classes expose protected hooks (`_buildValueInput`,
  `_renderOpsInto`, `_onSuccess`) for incremental customization.

**Standalone usability**: a `<crap-column-picker>` or
`<crap-filter-builder>` can be dropped anywhere with the right
dataset attributes — they don't depend on `<crap-list-settings>`
specifically, only on the data being attached to their own host.

### `<crap-relationship-search>` — relationship + upload picker (1075 LOC orchestrator + 197 LOC atom)

Status: **partially atomized (Phase 3.4).** The chip cluster
extracted as `<crap-pill-list>`. The rest stays as a composition.

| Tag                          | LOC  | Stable surface                                                              |
| ---------------------------- | ---- | --------------------------------------------------------------------------- |
| `<crap-relationship-search>` | 1075 | Host-element `data-*` attrs (collection, field-name, has-many, polymorphic, picker, etc.). Treat as a black box. |
| `<crap-pill-list>`           | 197  | `data-items` (JSON `Item[]`), `data-readonly`, `data-polymorphic`. Fires `crap:pill-removed` with `{ id }` detail. Self-styled (injects its own stylesheet). **Reusable** — drop into any has-many UI. |

**Honest about why we stopped at the pill list.** The earlier spec
proposed `<crap-search-input>` and `<crap-search-results>` as further
sub-atoms. Reading the actual code makes it clear those wouldn't be
genuine atoms — they'd share `this._selected`, `this._results`,
`this._collection` with the orchestrator and need every piece of state
threaded through attributes. The split would create more interfaces
without more independence. The whole search → fetch → render → select
loop is inherently one state machine; pretending otherwise costs more
in coordination than it saves in modularity.

The pill-list, in contrast, is naturally reusable: input is items,
output is "user wants this id removed." It works standalone, and
future has-many UIs (custom field types with chip-shaped selection,
new tag-style inputs) can use it directly.

**Stable contract today**: the host-element tag + the `data-*`
attributes used to bootstrap state. Internal HTML and classes are
private. Field-template overlays should target the component as a
black box. Inside the orchestrator, `<crap-pill-list>` is the one
piece overlay authors can swap independently — drop a replacement at
`<config_dir>/static/components/pill-list.js` to customize chip
rendering globally (affects every `<crap-relationship-search>` plus
any other component that adopts the atom).

## Helpers (not custom elements)

Imported by other modules; not registered with `customElements.define()`.

| Module             | Purpose                                                         |
| ------------------ | --------------------------------------------------------------- |
| `css.js`           | `css\`…\`` tagged-template returns a `CSSStyleSheet`.            |
| `h.js`             | `h(tag, props, ...children)` hyperscript helper.                 |
| `i18n.js`          | `t(key)` translation lookup; HTMX-swap cache invalidation.       |
| `events.js`        | Named event-string constants (this doc's source of truth).       |
| `theme.js`         | `getTheme()`, `setTheme()`, `applyTheme()`.                      |
| `groups.js`        | Field group/collapsible toggle handler (legacy; shared logic).   |
| `picker-base.js`   | Common base for `block-picker` and similar option-picker UI.     |
| `util/cookies.js`  | `readCsrfCookie()`.                                              |
| `util/discover.js` | `discoverSingleton(eventName)` for singleton-component discovery. |
| `util/htmx.js`     | `getHttpVerb(form)`.                                             |
| `util/json.js`     | `readDataIsland(id)`.                                            |
| `util/toast.js`    | `toast({ message, type, duration })` sugar over `EV_TOAST_REQUEST`. |
| `global.js`        | `window.crap.*` console / inline-template namespace.             |
| `index.js`         | Module manifest; imports every component for side effects.       |

These don't have a tag-shaped public API — their stability comes from
the export shape (function signatures, returned types).

## Stability tiers

Every component module's JSDoc declares one of three tiers via a
`@stability` tag at the bottom of the module-level block. Tier dictates
breaking-change policy:

- **`stable`** — public API listed above; breaking changes need a
  deprecation cycle, never inside a minor release.
- **`experimental`** — usable, but the API may change between minors.
- **`internal`** — anything not listed in this doc. Free to change.

The current distribution (post-Phase-3.4):

| Tier         | Count | Components                                                                                                                                                                                                                                                                                                                                          | Policy                                          |
| ------------ | ----- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------- |
| stable       | 32    | 6 singletons + 5 form atoms + 17 page-enhancement atoms + 4 newly-extracted atoms (`<crap-array-row>`, `<crap-pill-list>`, `<crap-column-picker>`, `<crap-filter-builder>`) + `events.js`                                                                                                                                                                | Back-compat enforced; breaking changes get a deprecation cycle. |
| experimental | 3     | `array-fields`, `relationship-search`, `list-settings` (the three composition orchestrators)                                                                                                                                                                                                                                                            | Internal HTML / orchestration shape can change. The host-element tag + documented `data-*` attributes stay stable so field templates that emit them keep working. |
| internal     | 20    | Helpers (`css`, `h`, `i18n`, `groups`, `picker-base`), `util/` subtree, `global.js`, `index.js`, seven `richtext/*` ProseMirror submodules                                                                                                                                                                                                              | Free to change without notice; not part of the public surface. |

Lookup with `grep -l "@stability stable" static/components/*.js` to
list every public-API file. The `@stability` tag is the single
machine-readable source of truth for tooling that wants to enforce
tier policy (lint rules, codemods, doc-extractor scripts).

## Out of scope

- The richtext editor's ProseMirror integration. ProseMirror has its
  own plugin/schema API; treat `<crap-richtext>` as a thin adapter and
  document `data-format`, `data-features`, `data-nodes` as the public
  surface. The schema-extension story is documented separately.
- The CodeMirror integration in `<crap-code>` similarly.

These wrap third-party editors with their own contracts; we keep our
adapter narrow and stable, and direct users to the upstream docs for
deep customization.
