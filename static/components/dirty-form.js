/**
 * Dirty Form Guard — warns users before navigating away from unsaved changes.
 *
 * Tracks changes on #edit-form via input/change events, custom crap:change
 * events (relationship/upload components), and array/block row mutations.
 * Guards HTMX navigation (configRequest), browser history (popstate), and
 * native navigation (beforeunload) using the styled <crap-confirm-dialog>.
 */

import { registerInit } from './actions.js';
import { getConfirmDialog } from './confirm-dialog.js';
import { t } from './i18n.js';

/** @type {boolean} */
let dirty = false;

/** @type {boolean} */
let bypassing = false;

/** @type {string} */
let formUrl = '';

function markDirty() { dirty = true; }

/**
 * Show the styled leave-page dialog.
 * @returns {Promise<boolean>}
 */
function askLeave() {
  return getConfirmDialog().prompt(
    t('unsaved_changes'),
    { confirmLabel: t('leave'), cancelLabel: t('stay') },
  );
}

function init() {
  const form = document.getElementById('edit-form');
  if (!form) { dirty = false; return; }
  dirty = false;
  formUrl = location.href;

  form.addEventListener('input', markDirty);
  form.addEventListener('change', markDirty);
}

// Custom component changes (relationship search, uploads)
document.addEventListener('crap:change', markDirty);

// Array/block row mutations — action buttons
document.addEventListener('click', (e) => {
  const action = e.target.closest('[data-action]');
  if (!action) return;
  const name = action.getAttribute('data-action');
  if (['remove-array-row', 'add-array-row', 'duplicate-row',
       'move-up', 'move-down'].includes(name)) {
    dirty = true;
  }
});

// Intercept HTMX GET navigation when form is dirty
document.addEventListener('htmx:configRequest', (e) => {
  if (!dirty || bypassing) return;
  if ((e.detail.verb || '').toUpperCase() !== 'GET') return;
  if (!document.getElementById('edit-form')) return;

  e.preventDefault();
  askLeave().then((confirmed) => {
    if (confirmed) {
      dirty = false;
      bypassing = true;
      window.location.href = e.detail.path;
      setTimeout(() => { bypassing = false; }, 500);
    }
  }).catch(() => {
    // Safety net — if dialog fails, don't permanently lock navigation
    dirty = false;
  });
});

// Intercept browser back/forward when form is dirty
window.addEventListener('popstate', () => {
  if (!dirty || bypassing) return;

  // Browser already changed the URL — push it back to stay on the form page
  history.pushState(null, '', formUrl);

  askLeave().then((confirmed) => {
    if (confirmed) {
      dirty = false;
      bypassing = true;
      history.back();
      setTimeout(() => { bypassing = false; }, 500);
    }
  }).catch(() => {
    dirty = false;
  });
});

// Clear dirty on form save (non-GET = POST/PUT submit)
document.addEventListener('htmx:beforeRequest', (e) => {
  if ((e.detail.verb || '').toUpperCase() !== 'GET') {
    dirty = false;
  }
});

// Native navigation guard (close tab, external URL)
window.addEventListener('beforeunload', (e) => {
  if (dirty) { e.preventDefault(); }
});

registerInit(init);
