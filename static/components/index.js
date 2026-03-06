/**
 * Crap CMS Components — ES module entry point.
 *
 * Pure import manifest. Each module is self-contained: web components
 * register themselves, behaviors bind their own listeners, actions
 * register via the delegation system.
 *
 * To override a single component, place a replacement file at the same
 * path in your config directory's static/ folder (overlay pattern).
 */

// ── i18n ──
import './i18n.js';

// ── Event delegation ──
import './actions.js';

// ── Web Components ──
import './toast.js';
import './confirm.js';
import './confirm-dialog.js';
import './richtext.js';
import './code.js';
import './tags.js';
import './drawer.js';
import './relationship-search.js';

// ── Behaviors ──
import './sidebar-toggle.js';
import './session-guard.js';
import './dirty-form.js';
import './uploads.js';
import './focal-point.js';
import './block-picker.js';
import './conditions.js';
import './live-events.js';
import './time-format.js';
import './scroll.js';
import './theme.js';
import './locale.js';
import './locale-picker.js';

// ── Actions ──
import './list-settings.js';
import './tabs.js';
import './groups.js';
import './array-fields.js';
