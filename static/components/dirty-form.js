/**
 * Dirty Form Guard — `<crap-dirty-form>`.
 *
 * Warns users before navigating away from unsaved changes on the
 * `#edit-form` it wraps. Tracks input/change events, custom
 * `crap:change` events from child components, and array/block row
 * mutations. Intercepts HTMX GET navigation, browser back/forward, and
 * tab close.
 *
 * @module dirty-form
 */

import { t } from './i18n.js';
import { getHttpVerb } from './util/htmx.js';

/**
 * Array/blocks row actions that should mark the form dirty when the
 * user clicks them.
 */
const DIRTY_ROW_ACTIONS = new Set([
  'remove-array-row',
  'add-array-row',
  'duplicate-row',
  'move-row-up',
  'move-row-down',
]);

/** ms to keep `_bypassing` true after triggering a programmatic navigation. */
const BYPASS_GRACE_MS = 500;

class CrapDirtyForm extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._dirty = false;
    /** @type {boolean} */
    this._bypassing = false;
    /** @type {boolean} */
    this._armed = false;
    /** @type {boolean} */
    this._connected = false;
    /** @type {string} */
    this._formUrl = '';
    /** @type {HTMLElement|null} */
    this._form = null;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    this._formUrl = location.href;
    this._dirty = false;

    // Defer arming until after child components finish initialising. Without
    // this, `crap:change` events fired during `<crap-relationship-search>`
    // setup would mark the form dirty before the user touched anything.
    requestAnimationFrame(() => {
      this._armed = true;
    });

    this._markDirty = () => {
      if (this._armed) this._dirty = true;
    };

    this._form = this.querySelector('#edit-form');
    if (this._form) {
      this._form.addEventListener('input', this._markDirty);
      this._form.addEventListener('change', this._markDirty);
    }

    // `crap:change` is the agreed signal from custom inputs (relationship,
    // uploads, tags) that don't fire native input/change.
    this.addEventListener('crap:change', this._markDirty);

    this._onRowAction = (e) => {
      if (!this._armed) return;
      const target = e.target;
      if (!(target instanceof Element)) return;
      const action = target.closest('[data-action]');
      if (!action) return;
      const name = action.getAttribute('data-action') || '';
      if (DIRTY_ROW_ACTIONS.has(name)) this._dirty = true;
    };
    document.addEventListener('click', this._onRowAction);

    this._onConfigRequest = (e) => this._onHtmxConfigRequest(e);
    document.addEventListener('htmx:configRequest', this._onConfigRequest);

    this._onPopState = () => this._onBrowserNav();
    window.addEventListener('popstate', this._onPopState);

    this._onBeforeRequest = (e) => {
      // Form save (non-GET = POST/PUT/DELETE/PATCH) clears the dirty flag.
      if (getHttpVerb(e) !== 'GET') this._dirty = false;
    };
    document.addEventListener('htmx:beforeRequest', this._onBeforeRequest);

    this._onBeforeUnload = (e) => {
      if (this._dirty) e.preventDefault();
    };
    window.addEventListener('beforeunload', this._onBeforeUnload);
  }

  disconnectedCallback() {
    this._connected = false;
    if (this._form && this._markDirty) {
      this._form.removeEventListener('input', this._markDirty);
      this._form.removeEventListener('change', this._markDirty);
      this._form = null;
    }
    if (this._markDirty) this.removeEventListener('crap:change', this._markDirty);
    if (this._onRowAction) document.removeEventListener('click', this._onRowAction);
    if (this._onConfigRequest)
      document.removeEventListener('htmx:configRequest', this._onConfigRequest);
    if (this._onPopState) window.removeEventListener('popstate', this._onPopState);
    if (this._onBeforeRequest)
      document.removeEventListener('htmx:beforeRequest', this._onBeforeRequest);
    if (this._onBeforeUnload) window.removeEventListener('beforeunload', this._onBeforeUnload);
  }

  /**
   * HTMX `htmx:configRequest` listener. Intercept GET navigations away
   * from the edit form and prompt before letting them through.
   *
   * @param {Event} e
   */
  async _onHtmxConfigRequest(e) {
    if (!this._dirty || this._bypassing) return;
    if (getHttpVerb(e) !== 'GET') return;
    if (!this.querySelector('#edit-form')) return;

    e.preventDefault();
    const evt = /** @type {CustomEvent} */ (e);
    if (!(await this._askLeave())) return;
    this._bypassNavigate(() => {
      window.location.href = evt.detail.path;
    });
  }

  /**
   * `popstate` listener. Browser back/forward — re-push current URL,
   * prompt, then go back if confirmed.
   */
  async _onBrowserNav() {
    if (!this._dirty || this._bypassing) return;
    history.pushState(null, '', this._formUrl);
    if (!(await this._askLeave())) return;
    this._bypassNavigate(() => history.back());
  }

  /**
   * Run `action` with the dirty flag cleared and `_bypassing` set so
   * the next interception cycle lets the navigation through.
   *
   * @param {() => void} action
   */
  _bypassNavigate(action) {
    this._dirty = false;
    this._bypassing = true;
    action();
    setTimeout(() => {
      this._bypassing = false;
    }, BYPASS_GRACE_MS);
  }

  /**
   * Discover the page's `<crap-confirm-dialog>` and prompt with an
   * "unsaved changes" message. If no dialog is registered, allow the
   * navigation (returns `true`).
   *
   * @returns {Promise<boolean>}
   */
  _askLeave() {
    const evt = new CustomEvent('crap:confirm-dialog-request', { detail: {} });
    document.dispatchEvent(evt);
    const dialog = evt.detail.instance;
    if (!dialog) return Promise.resolve(true);
    return dialog.prompt(t('unsaved_changes'), {
      confirmLabel: t('leave'),
      cancelLabel: t('stay'),
    });
  }
}

customElements.define('crap-dirty-form', CrapDirtyForm);
