/**
 * Dirty Form Guard — `<crap-dirty-form>`.
 *
 * Warns users before navigating away from unsaved changes on the
 * `#edit-form` it wraps. Tracks input/change events and the custom
 * `crap:change` events child components (array/block rows included)
 * announce their edits with. Intercepts HTMX GET navigation, browser back/forward, and
 * tab close.
 *
 * @attr data-unsaved  Start out dirty: the server re-rendered a submission
 *                     it did not save (a validation error), so the form
 *                     already holds unsaved input.
 *
 * @module dirty-form
 * @category form-field
 * @stability stable
 */

import { t } from './_internal/i18n.js';
import { getHttpVerb } from './_internal/util/htmx.js';
import { EV_CHANGE, EV_CONFIRM_DIALOG_REQUEST } from './events.js';

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
    this._dirty = this.hasAttribute('data-unsaved');

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
    // uploads, tags) that don't fire native input/change, and from
    // `<crap-array-field>` after every row mutation (add — the card picker
    // included —, remove, duplicate, move, drag-and-drop).
    this.addEventListener(EV_CHANGE, this._markDirty);

    this._onUnpublishClick = (e) => {
      const btn =
        e.target instanceof Element ? e.target.closest('[data-action="unpublish"]') : null;
      if (!btn || !this.contains(btn)) return;
      e.preventDefault();
      this._unpublish();
    };
    this.addEventListener('click', this._onUnpublishClick);

    this._onConfigRequest = (e) => this._onHtmxConfigRequest(e);
    document.addEventListener('htmx:configRequest', this._onConfigRequest);

    this._onPopState = () => this._onBrowserNav();
    window.addEventListener('popstate', this._onPopState);

    // A save (non-GET = POST/PUT/DELETE/PATCH) clears the dirty flag only
    // once the server has accepted it — and only for THIS form's own
    // request (an inline-create panel posting its own form must not clear
    // it). `htmx:beforeOnLoad` fires before htmx acts on the response, so the
    // flag is already clear when a success follows `HX-Redirect` (no leave
    // prompt for the save itself). An error status (422 validation or hook
    // error, 403, 409, 413) leaves the form as it was — still dirty. A 200
    // validation re-render swaps in a fresh guard marked `data-unsaved`.
    this._onBeforeOnLoad = (e) => {
      const detail = /** @type {CustomEvent} */ (e).detail;
      const elt = detail?.elt;
      if (!this._form || !(elt instanceof Node) || !this._form.contains(elt)) return;
      if (getHttpVerb(e) === 'GET') return;

      const status = /** @type {XMLHttpRequest|undefined} */ (detail.xhr)?.status ?? 0;
      if (status >= 200 && status < 400) this._dirty = false;
    };
    document.addEventListener('htmx:beforeOnLoad', this._onBeforeOnLoad);

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
    if (this._markDirty) this.removeEventListener(EV_CHANGE, this._markDirty);
    if (this._onUnpublishClick) this.removeEventListener('click', this._onUnpublishClick);
    if (this._onConfigRequest)
      document.removeEventListener('htmx:configRequest', this._onConfigRequest);
    if (this._onPopState) window.removeEventListener('popstate', this._onPopState);
    if (this._onBeforeOnLoad)
      document.removeEventListener('htmx:beforeOnLoad', this._onBeforeOnLoad);
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
   * Ask before a navigation a component starts itself (the editor locale
   * picker's reload): resolves `true` when nothing is unsaved or the editor
   * chose to leave — clearing the flag, so the unload that follows does not
   * ask a second time — and `false` when they chose to stay. The caller acts
   * (writes its cookie, reloads) only on `true`.
   *
   * @returns {Promise<boolean>}
   */
  async confirmLeave() {
    if (!this._dirty) return true;
    if (!(await this._askLeave())) return false;
    this._dirty = false;
    return true;
  }

  /**
   * The Unpublish action. It is not a save: it takes the document out of
   * publication and keeps nothing the form holds. So it posts only the
   * action (and the form's meta inputs — locale, method, revision), never the
   * form's fields, and runs no pre-submit validation of fields it ignores.
   * When the form holds unsaved edits the editor is asked first: they would
   * be discarded, so the choice is to discard them and unpublish, or to stay
   * and save first.
   */
  async _unpublish() {
    const form = this._form;
    const url = form?.getAttribute('action');
    if (!form || !url) return;
    if (this._dirty && !(await this._askDiscardForUnpublish())) return;

    /** @type {Record<string, string>} */
    const values = { _action: 'unpublish' };
    for (const name of ['_method', '_locale', '_revision']) {
      const input = /** @type {HTMLInputElement|null} */ (
        form.querySelector(`:scope > input[name="${name}"]`)
      );
      if (input) values[name] = input.value;
    }

    this._bypassNavigate(() => {
      htmx.ajax('POST', url, { values, target: '#main', swap: 'innerHTML show:window:top' });
    });
  }

  /**
   * Ask whether to discard the unsaved edits and unpublish.
   *
   * @returns {Promise<boolean>}
   */
  _askDiscardForUnpublish() {
    const evt = new CustomEvent(EV_CONFIRM_DIALOG_REQUEST, { detail: {} });
    document.dispatchEvent(evt);
    const dialog = evt.detail.instance;
    const message = t('unpublish_unsaved_changes');
    if (!dialog) return Promise.resolve(window.confirm(message));
    return dialog.prompt(message, {
      confirmLabel: t('discard_and_unpublish'),
      cancelLabel: t('stay'),
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
    const evt = new CustomEvent(EV_CONFIRM_DIALOG_REQUEST, { detail: {} });
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
