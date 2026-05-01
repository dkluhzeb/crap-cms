# Scenario 3: Replace the toast component

**Goal**: change how `<crap-toast>` looks or behaves — different
animation, different layout, audit logging, etc.

**Difficulty**: depends on goal. Adding behavior is one line in
`custom.js` (5 minutes). Replacing the rendering entirely is a fork
of `static/components/toast.js` (~30 minutes plus ongoing
maintenance).

**You'll touch**: `static/components/custom.js` (for behavior add),
or `static/components/toast.js` (for full replacement).

## Approach

There are three paths, each suited to a different goal:

1. **Add behavior** — listen for the public event, do your thing,
   let the original component continue. *Strictly additive.*
   Inherits all upstream improvements automatically. This is the
   recommended path for ~95% of "I want X to happen when toast
   shows" goals.

2. **Replace rendering** — drop your own `toast.js`. Your version
   completely supersedes upstream. You inherit no upstream
   improvements until you re-port them. Use when you want full
   control over the markup and animation.

3. **Override CSS only** — drop `toast.js`-specific CSS variables
   in your theme. Use when you only want to tweak the visual.

## Path 1 — add behavior via capture-phase listener

Goal example: log every toast to a backend audit endpoint.

Drop a single file in your config dir:

```js
// <config_dir>/static/components/custom.js
document.addEventListener('crap:toast-request', (e) => {
  fetch('/api/audit/toast', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      message: e.detail.message,
      type: e.detail.type ?? 'info',
      timestamp: Date.now(),
    }),
  });
  // Don't stop propagation — upstream's <crap-toast> still gets the
  // event and shows the toast normally. Your listener runs alongside.
}, true /* capture phase */);
```

`custom.js` is auto-imported from `index.js` if it exists in your
overlay. No registration needed.

This is **strictly additive** — you didn't replace the toast
component, so upstream improvements (animation tweaks, queue
fixes, accessibility updates) flow through automatically.

See [events reference](../reference/events.md) for the full list of
public events you can listen to this way.

## Path 2 — full replacement

Goal example: replace the toast UI with a wholly different design
(e.g., a banner at the top instead of corner toasts).

```
$ crap-cms templates extract components/toast.js
```

This copies the embedded `toast.js` into your config dir at
`<config_dir>/static/components/toast.js` with a source-version
header.

Edit the file freely. Your version completely replaces upstream's.
Some constraints:

- **Keep the tag name `crap-toast`** — built-in code dispatches
  `crap:toast-request` events expecting `<crap-toast>` to be
  registered (HTMX response headers like `X-Crap-Toast` also flow
  through it).
- **Listen for the same public event** (`crap:toast-request`) so
  built-in calls keep working. Import the constant from
  `events.js` rather than re-typing the string:

  ```js
  import { EV_TOAST_REQUEST } from './events.js';
  ```

- **Register the tag at the bottom of the file**, mirroring how
  upstream's `toast.js` does it:

  ```js
  customElements.define('crap-toast', YourToastClass);
  ```

  No defensive `if (!customElements.get(...))` guard is needed for
  a full overlay replacement — the overlay handler serves your file
  when the URL is `/static/components/toast.js`, so upstream's
  registration code never runs.

After overriding, `crap-cms templates status` will show:

```
static/components/toast.js   ✓ current
```

…until you upgrade crap-cms. After upgrade:

```
static/components/toast.js   ⚠ behind: extracted from 0.1.0-alpha.8
```

Run `crap-cms templates diff components/toast.js` to see what
upstream changed and re-port the bits that matter.

## Path 3 — CSS-only override

If you only want to tweak the visual (colors, spacing, fonts)
without changing behavior, override the CSS tokens. Toast inherits
from the same theme tokens as the rest of the admin:

```css
/* <config_dir>/static/styles/themes/themes-acme.css */
html[data-theme="acme"] {
  /* These tokens flow into toast's constructable stylesheet via
     `var(--…)` references — change them and toast picks them up. */
  --color-primary: #ff5500;
  --color-primary-bg: rgba(255, 85, 0, 0.08);
  --color-success: #2ea043;
  --color-danger: #d62828;

  /* Spacing + radius cascade to toast as well */
  --space-md: 0.5rem;
  --radius-md: 0;          /* square corners */
}
```

See [CSS variables reference](../reference/css-variables.md) for
the full token catalogue and [Themes guide](../guides/themes.md)
for the registration pattern.

> **Note**: Built-in components don't currently expose CSS
> [`::part()`](https://developer.mozilla.org/en-US/docs/Web/CSS/::part)
> selectors — there are no `part="…"` attributes on the internal
> Shadow-DOM elements yet. Theming via CSS custom properties is the
> only token-level external customization path today. For finer
> structural control, fall through to Path 2.

## Choosing between paths

| Goal | Path |
|---|---|
| Audit logging on every toast | Path 1 (listener) |
| Send toast events to analytics | Path 1 |
| Block certain toast messages | Path 1 (with `e.stopPropagation()` in your handler) |
| Change the toast colors / spacing / radius | Path 3 (theme tokens) |
| Change the toast animation timing or motion | Path 2 (the timing is internal to toast.js) |
| Change the toast layout (banner vs. corner) | Path 2 |
| Change the toast icon set | Path 2 (icons are inline in toast's render) |
| Show toasts as native browser notifications | Path 1 — call `Notification.requestPermission()` and `new Notification(e.detail.message)` |

Most "I want X to happen" goals are Path 1. Reach for Path 2 only
when you're willing to take on the maintenance burden of a fork.

## Verifying

```
$ crap-cms templates status
```

For Path 1, `custom.js` shows as `· user-original` — no drift to
track.

For Path 2, `toast.js` shows as `✓ current` initially, then
`⚠ behind` after upgrade. That's your signal to run `templates
diff` and re-port.
