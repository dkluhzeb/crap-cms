/**
 * <crap-confirm-dialog> — Standalone confirmation dialog for HTMX actions.
 *
 * Replaces the native `window.confirm()` used by `hx-confirm` with a
 * styled dialog. All styles are encapsulated in Shadow DOM. CSS custom
 * properties from `:root` pierce the shadow boundary so the dialog
 * follows the active theme.
 *
 * Instance-safe: each connected instance registers its own
 * `htmx:confirm` listener via `connectedCallback`/`disconnectedCallback`.
 *
 * Usage: add `hx-confirm="Are you sure?"` to any HTMX-powered element.
 *
 * @module confirm-dialog
 * @stability stable
 */

import { css } from './_internal/css.js';
import { h } from './_internal/h.js';
import { t } from './_internal/i18n.js';
import { EV_CONFIRM_DIALOG_REQUEST } from './events.js';

const sheet = css`
  :host { display: contents; }
  dialog {
    border: none;
    border-radius: var(--radius-xl, 12px);
    padding: 0;
    max-width: 25rem;
    width: 90vw;
    box-shadow: var(--shadow-lg, 0 16px 48px rgba(0, 0, 0, 0.2));
    font-family: inherit;
    background: var(--bg-elevated, #fff);
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
  }
  dialog::backdrop { background: rgba(0, 0, 0, 0.4); }
  .body { padding: var(--space-xl, 1.5rem); }
  .body p {
    margin: 0;
    font-size: var(--text-sm, 0.8125rem);
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
    line-height: 1.5;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-sm, 0.5rem);
    padding: 0 var(--space-xl, 1.5rem) var(--space-xl, 1.5rem);
  }
  button {
    font-family: inherit;
    font-size: var(--text-sm, 0.8125rem);
    font-weight: 500;
    height: var(--button-height, 2.25rem);
    padding: 0 var(--space-lg, 1rem);
    border-radius: var(--radius-md, 6px);
    border: none;
    cursor: pointer;
    transition: background var(--transition-fast, 0.15s ease);
  }
  .cancel {
    background: transparent;
    color: var(--text-secondary, rgba(0, 0, 0, 0.65));
    border: 1px solid var(--border-color-hover, #d9d9d9);
  }
  .cancel:hover { background: var(--bg-hover, rgba(0, 0, 0, 0.04)); }
  .confirm {
    background: var(--color-danger, #dc2626);
    color: var(--text-on-primary, #fff);
  }
  .confirm:hover { background: var(--color-danger-hover, #ef4444); }
`;

/** @typedef {{ confirmLabel?: string, cancelLabel?: string }} PromptOptions */

class CrapConfirmDialog extends HTMLElement {
  constructor() {
    super();
    const root = this.attachShadow({ mode: 'open' });
    root.adoptedStyleSheets = [sheet];

    /** @type {HTMLParagraphElement} */
    this._messageEl = h('p');
    /** @type {HTMLButtonElement} */
    this._cancelBtn = h('button', { class: 'cancel', type: 'button', text: t('cancel') });
    /** @type {HTMLButtonElement} */
    this._confirmBtn = h('button', { class: 'confirm', type: 'button', text: t('confirm') });
    /** @type {HTMLDialogElement} */
    this._dialog = h(
      'dialog',
      null,
      h('div', { class: 'body' }, this._messageEl),
      h('div', { class: 'actions' }, this._cancelBtn, this._confirmBtn),
    );
    root.append(this._dialog);

    /** @type {((e: Event) => void)|null} */
    this._handleRequest = null;
    /** @type {((e: Event) => void)|null} */
    this._handleHtmxConfirm = null;
  }

  /**
   * Show the dialog with `message` and resolve to `true` on confirm,
   * `false` on cancel or ESC.
   *
   * Listeners self-clean via an `AbortController` — when the controller
   * aborts, the click + dialog-cancel handlers are removed in one step.
   *
   * @param {string} message
   * @param {PromptOptions} [opts]
   * @returns {Promise<boolean>}
   */
  prompt(message, opts = {}) {
    this._messageEl.textContent = message;
    this._cancelBtn.textContent = opts.cancelLabel ?? t('cancel');
    this._confirmBtn.textContent = opts.confirmLabel ?? t('confirm');

    return new Promise((resolve) => {
      const ctrl = new AbortController();
      const settle = (value) => {
        ctrl.abort();
        resolve(value);
      };

      this._cancelBtn.addEventListener(
        'click',
        () => {
          this._dialog.close();
          settle(false);
        },
        { signal: ctrl.signal },
      );

      this._confirmBtn.addEventListener(
        'click',
        () => {
          this._dialog.close();
          settle(true);
        },
        { signal: ctrl.signal },
      );

      // ESC key fires `cancel` and auto-closes the dialog.
      this._dialog.addEventListener('cancel', () => settle(false), { signal: ctrl.signal });

      this._dialog.showModal();
    });
  }

  connectedCallback() {
    // Respond to `getConfirmDialog()` discovery requests.
    this._handleRequest = (e) => {
      const detail = /** @type {CustomEvent} */ (e).detail;
      if (!detail.instance) detail.instance = this;
    };
    document.addEventListener(EV_CONFIRM_DIALOG_REQUEST, this._handleRequest);

    // Intercept HTMX's native confirm and show the styled dialog instead.
    this._handleHtmxConfirm = async (e) => {
      const evt = /** @type {any} */ (e);
      if (evt._crapHandled) return;
      const question = evt.detail.question;
      if (!question) return;

      evt._crapHandled = true;
      evt.preventDefault();
      const confirmed = await this.prompt(question);
      if (confirmed) evt.detail.issueRequest();
    };
    document.addEventListener('htmx:confirm', this._handleHtmxConfirm);
  }

  disconnectedCallback() {
    if (this._handleRequest) {
      document.removeEventListener(EV_CONFIRM_DIALOG_REQUEST, this._handleRequest);
    }
    if (this._handleHtmxConfirm) {
      document.removeEventListener('htmx:confirm', this._handleHtmxConfirm);
    }
  }
}

customElements.define('crap-confirm-dialog', CrapConfirmDialog);
