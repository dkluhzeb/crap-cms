/**
 * Crap CMS components — ES module entry point.
 *
 * Pure import manifest. Each module is a self-contained Web Component
 * that registers itself via `customElements.define()` at evaluation
 * time. Import order doesn't matter for correctness; the sections are
 * grouped + alpha-sorted only as a navigation aid.
 *
 * **Override pattern**: drop a replacement file at the matching path
 * inside your config directory's `static/` folder. The admin overlay
 * resolves config-dir paths first, compiled defaults second.
 *
 * @module index
 */

// ── i18n helper ──
// Other components import `t` from this module, so its side-effect
// listener (HTMX-swap cache invalidation) registers transitively.
// The explicit import keeps the registration guaranteed even if a
// future override swaps every consumer.
import './i18n.js';

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
import './richtext.js';
import './session-guard.js';
import './tags.js';
import './toast.js';

// ── Light DOM Web Components ──────────────────────────────────────
// Operate on server-rendered markup. Styles either live in the global
// stylesheet or — for components that need page-level CSS for their
// rendered output — push a sheet onto `document.adoptedStyleSheets`
// once at first connect (e.g. relationship-search, create-panel).
import './array-fields.js';
import './conditions.js';
import './create-panel.js';
import './dirty-form.js';
import './groups.js';
import './list-settings.js';
import './live-events.js';
import './locale-picker.js';
import './password-toggle.js';
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
import './global.js';
