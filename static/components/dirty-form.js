/**
 * Dirty Form Guard — `<crap-dirty-form>`.
 *
 * Warns users before navigating away from unsaved changes.
 * Tracks changes on #edit-form via input/change events, custom crap:change
 * events, and array/block row mutations.
 *
 * @module dirty-form
 */

import { getConfirmDialog } from './confirm-dialog.js';
import { t } from './i18n.js';

class CrapDirtyForm extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._dirty = false;
    /** @type {boolean} */
    this._bypassing = false;
    /** @type {string} */
    this._formUrl = '';
  }

  connectedCallback() {
    this._formUrl = location.href;
    this._dirty = false;
    /** @type {boolean} */
    this._armed = false;

    this._markDirty = () => { if (this._armed) this._dirty = true; };

    // Defer arming until after all child components have initialized.
    // This prevents crap:change events fired during <crap-relationship-search>
    // setup from marking the form dirty.
    requestAnimationFrame(() => { this._armed = true; });

    // Track form input/change
    const form = this.querySelector('#edit-form');
    if (form) {
      form.addEventListener('input', this._markDirty);
      form.addEventListener('change', this._markDirty);
    }

    // Custom component changes (relationship search, uploads)
    this.addEventListener('crap:change', this._markDirty);

    // Array/block row mutations
    this._onRowAction = (e) => {
      if (!this._armed) return;
      const action = /** @type {HTMLElement} */ (e.target).closest('[data-action]');
      if (!action) return;
      const name = action.getAttribute('data-action');
      if (['remove-array-row', 'add-array-row', 'duplicate-row',
           'move-row-up', 'move-row-down'].includes(name)) {
        this._dirty = true;
      }
    };
    document.addEventListener('click', this._onRowAction);

    // Intercept HTMX GET navigation when form is dirty
    this._onConfigRequest = (e) => {
      if (!this._dirty || this._bypassing) return;
      if ((e.detail.verb || '').toUpperCase() !== 'GET') return;
      if (!this.querySelector('#edit-form')) return;

      e.preventDefault();
      this._askLeave().then((confirmed) => {
        if (confirmed) {
          this._dirty = false;
          this._bypassing = true;
          window.location.href = e.detail.path;
          setTimeout(() => { this._bypassing = false; }, 500);
        }
      }).catch(() => { this._dirty = false; });
    };
    document.addEventListener('htmx:configRequest', this._onConfigRequest);

    // Intercept browser back/forward
    this._onPopState = () => {
      if (!this._dirty || this._bypassing) return;
      history.pushState(null, '', this._formUrl);
      this._askLeave().then((confirmed) => {
        if (confirmed) {
          this._dirty = false;
          this._bypassing = true;
          history.back();
          setTimeout(() => { this._bypassing = false; }, 500);
        }
      }).catch(() => { this._dirty = false; });
    };
    window.addEventListener('popstate', this._onPopState);

    // Clear dirty on form save (non-GET = POST/PUT submit)
    this._onBeforeRequest = (e) => {
      if ((e.detail.verb || '').toUpperCase() !== 'GET') {
        this._dirty = false;
      }
    };
    document.addEventListener('htmx:beforeRequest', this._onBeforeRequest);

    // Native navigation guard (close tab, external URL)
    this._onBeforeUnload = (e) => {
      if (this._dirty) e.preventDefault();
    };
    window.addEventListener('beforeunload', this._onBeforeUnload);
  }

  disconnectedCallback() {
    this.removeEventListener('crap:change', this._markDirty);
    document.removeEventListener('click', this._onRowAction);
    document.removeEventListener('htmx:configRequest', this._onConfigRequest);
    window.removeEventListener('popstate', this._onPopState);
    document.removeEventListener('htmx:beforeRequest', this._onBeforeRequest);
    window.removeEventListener('beforeunload', this._onBeforeUnload);
  }

  /**
   * @returns {Promise<boolean>}
   */
  _askLeave() {
    return getConfirmDialog().prompt(
      t('unsaved_changes'),
      { confirmLabel: t('leave'), cancelLabel: t('stay') },
    );
  }
}

customElements.define('crap-dirty-form', CrapDirtyForm);
