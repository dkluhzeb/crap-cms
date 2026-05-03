/**
 * <crap-confirm> — Confirmation guard around destructive form actions.
 *
 * Intercepts `submit` events from a slotted child form, asks the user
 * to confirm via the page-singleton `<crap-confirm-dialog>`, and only
 * re-submits if confirmed.
 *
 * For standalone HTMX buttons (not inside a child form), use
 * `hx-confirm` — `<crap-confirm-dialog>` handles those directly.
 *
 * @attr message - Confirmation prompt text (default: "Are you sure?").
 *
 * @example
 * <crap-confirm message="Delete this item permanently?">
 *   <form method="post" action="/delete/123">
 *     <button type="submit" class="button button--danger">Delete</button>
 *   </form>
 * </crap-confirm>
 *
 * @module confirm
 * @stability stable
 */

import { t } from './_internal/i18n.js';
import { discoverSingleton } from './_internal/util/discover.js';
import { EV_CONFIRM_DIALOG_REQUEST } from './events.js';

class CrapConfirm extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    // Set to true between the confirm response and the resulting
    // requestSubmit so the re-fired submit event passes through.
    /** @type {boolean} */
    this._confirmed = false;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    // Capture phase so we run before HTMX's direct listener on the
    // child form. In the target phase HTMX's handler would fire first
    // and send the request before we could intercept.
    this.addEventListener('submit', (e) => this._onSubmit(e), true);
  }

  disconnectedCallback() {
    // Do NOT reset _connected — `submit` is listened on `this`, the
    // capture-phase listener survives DOM moves.
  }

  /** @param {Event} e */
  async _onSubmit(e) {
    if (this._confirmed) {
      this._confirmed = false;
      return; // let the confirmed re-submit through
    }
    e.preventDefault();
    e.stopImmediatePropagation();

    const form = /** @type {HTMLFormElement} */ (e.target);
    const message = this.getAttribute('message') || t('are_you_sure');
    if (!(await this._ask(message))) return;

    this._confirmed = true;
    form.requestSubmit();
  }

  /**
   * Discover the page's `<crap-confirm-dialog>` and prompt with
   * `message`. Falls back to native `window.confirm()` if no dialog
   * is mounted.
   *
   * @param {string} message
   * @returns {Promise<boolean>}
   */
  _ask(message) {
    const dialog = discoverSingleton(EV_CONFIRM_DIALOG_REQUEST);
    if (!dialog) {
      console.warn(
        '<crap-confirm>: no <crap-confirm-dialog> mounted; falling back to window.confirm()',
      );
      return Promise.resolve(window.confirm(message));
    }
    return dialog.prompt(message);
  }
}

customElements.define('crap-confirm', CrapConfirm);
