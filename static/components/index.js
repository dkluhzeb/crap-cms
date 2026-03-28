/**
 * Crap CMS Components — ES module entry point.
 *
 * Pure import manifest. Each module is a self-contained web component
 * that registers itself via `customElements.define()`.
 *
 * To override a single component, place a replacement file at the same
 * path in your config directory's static/ folder (overlay pattern).
 */

// ── i18n ──
import './i18n.js';

// ── Shadow DOM Web Components ──
import './toast.js';
import './confirm.js';
import './confirm-dialog.js';
import './delete-dialog.js';
import './richtext.js';
import './code.js';
import './tags.js';
import './drawer.js';
import './relationship-search.js';
import './session-guard.js';

// ── Light DOM Web Components ──
import './time-format.js';
import './theme.js';
import './locale-picker.js';
import './ui-locale-picker.js';
import './focal-point.js';
import './sidebar-toggle.js';
import './uploads.js';
import './groups.js';
import './tabs.js';
import './block-picker.js';
import './array-fields.js';
import './conditions.js';
import './dirty-form.js';
import './validate-form.js';
import './scroll.js';
import './live-events.js';
import './sticky-header.js';
import './list-settings.js';
