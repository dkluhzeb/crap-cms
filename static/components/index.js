/**
 * Crap CMS components — ES module entry point.
 *
 * Pure import manifest. Each module is a self-contained Web Component
 * that registers itself via `customElements.define()` at evaluation
 * time. Import order doesn't matter for correctness; the sections are
 * grouped + alpha-sorted only as a navigation aid.
 *
 * ## Layout convention
 *
 * - Public components live flat at `components/<name>.js` and form the
 *   override surface — drop a same-named file in your config dir's
 *   `static/components/` to override one.
 * - `_internal/` holds plumbing modules (`h`, `css`, `i18n`, helpers,
 *   `util/`). Underscore-prefix marks the namespace as framework-
 *   reserved; not part of the override contract.
 * - `custom.js` is an auto-imported user seam — drop one at
 *   `<config_dir>/static/components/custom.js` to register bespoke
 *   components without forking `index.js`. Most "extend a built-in
 *   component" goals are better solved by listening to its public
 *   event in capture phase from `custom.js` than by replacing the
 *   component file.
 *
 * @module index
 * @stability internal
 */

// ── i18n helper ──
// Other components import `t` from this module, so its side-effect
// listener (HTMX-swap cache invalidation) registers transitively.
// The explicit import keeps the registration guaranteed even if a
// future override swaps every consumer.
import './_internal/i18n.js';

// ── Shadow DOM Web Components ─────────────────────────────────────
// Encapsulated styling via constructable stylesheets.
import './back-refs.js';
import './block-picker.js';
import './code.js';
import './confirm-dialog.js';
import './confirm.js';
import './delete-dialog.js';
import './drawer.js';
import './focal-point.js';
import './password-toggle.js';
import './richtext.js';
import './session-guard.js';
import './tags.js';
import './toast.js';

// ── Light DOM Web Components ──────────────────────────────────────
// Operate on server-rendered markup. Styles either live in the global
// stylesheet or — for components that need page-level CSS for their
// rendered output — push a sheet onto `document.adoptedStyleSheets`
// once at first connect (e.g. relationship-search, create-panel,
// pill-list).
import './array-fields.js';
import './array-row.js';
import './conditions.js';
import './create-panel.js';
import './dirty-form.js';
import './_internal/groups.js';
import './list-settings.js';
import './list-settings/column-picker.js';
import './list-settings/filter-builder.js';
import './live-events.js';
import './locale-picker.js';
import './pill-list.js';
import './relationship-search.js';
import './scroll.js';
import './sidebar-toggle.js';
import './sticky-header.js';
import './tabs.js';
import './theme.js';
import './time-format.js';
import './ui-locale-picker.js';
import './uploads.js';
import './validate-form.js';

// ── window.crap namespace ─────────────────────────────────────────
// Convenience layer for inline scripts and console use. Imports last
// so all referenced singletons are registered before the namespace
// dispatches discovery events.
import './_internal/global.js';

// ── User seam ─────────────────────────────────────────────────────
// If the config dir overlays `static/components/custom.js`, register
// bespoke components there. The dynamic `import()` is wrapped in a
// `.catch(() => {})` so a missing file is a no-op, not a console
// error. The browser fetches the URL once; a 404 response yields a
// rejected promise that we silence.
import('./custom.js').catch(() => {});
