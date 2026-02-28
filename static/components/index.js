/**
 * Crap CMS Components — ES module entry point.
 *
 * Imports all component modules. Each module is self-contained:
 * web components register themselves, init functions bind their own
 * DOMContentLoaded/htmx:afterSettle listeners.
 *
 * Functions called from inline onclick handlers in templates are
 * assigned to window here so they're globally accessible.
 *
 * To override a single component, place a replacement file at the same
 * path in your config directory's static/ folder (overlay pattern).
 */

// Web components (self-registering)
import './toast.js';
import './confirm.js';
import './confirm-dialog.js';
import './richtext.js';

// Functional modules (self-initializing)
import './theme.js';
import './uploads.js';
import './relationships.js';
import './conditions.js';
import './live-events.js';
import './locale.js';
import './scroll.js';

// Modules with globals for template onclick handlers
import { toggleGroup } from './groups.js';
import { switchTab } from './tabs.js';
import {
  toggleArrayRow,
  toggleAllRows,
  addArrayRow,
  addBlockRow,
  removeArrayRow,
  moveRowUp,
  moveRowDown,
  duplicateRow,
  rowDragStart,
  rowDragEnd,
  rowDragOver,
  rowDrop,
} from './array-fields.js';

// Expose globals for inline onclick/ondrag handlers in templates
window.switchTab = switchTab;
window.toggleGroup = toggleGroup;
window.toggleArrayRow = toggleArrayRow;
window.toggleAllRows = toggleAllRows;
window.addArrayRow = addArrayRow;
window.addBlockRow = addBlockRow;
window.removeArrayRow = removeArrayRow;
window.moveRowUp = moveRowUp;
window.moveRowDown = moveRowDown;
window.duplicateRow = duplicateRow;
window.rowDragStart = rowDragStart;
window.rowDragEnd = rowDragEnd;
window.rowDragOver = rowDragOver;
window.rowDrop = rowDrop;
