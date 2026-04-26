/**
 * <crap-confirm> — Confirmation dialog that wraps destructive form actions.
 *
 * Intercepts `submit` events from a slotted child form, shows a styled
 * dialog, and only re-submits if the user confirms.
 *
 * For standalone HTMX buttons (not inside a child form), use
 * `hx-confirm` — `<crap-confirm-dialog>` handles those.
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
 */

import { css } from './css.js';
import { h } from './h.js';
import { t } from './i18n.js';

const sheet = css`
  :host { display: contents; }
  dialog {
    border: none;
    border-radius: var(--radius-xl, 12px);
    padding: 0;
    max-width: 400px;
    width: 90vw;
    box-shadow: var(--shadow-lg, 0 16px 48px rgba(0, 0, 0, 0.2));
    font-family: inherit;
    background: var(--bg-elevated, #fff);
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
  }
  dialog::backdrop { background: rgba(0, 0, 0, 0.4); }
  .dialog__body { padding: var(--space-xl, 1.5rem); }
  .dialog__body p {
    margin: 0;
    font-size: var(--text-base, 0.875rem);
    color: var(--text-primary, rgba(0, 0, 0, 0.8));
    line-height: 1.5;
  }
  .dialog__actions {
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
  .btn-cancel {
    background: transparent;
    color: var(--text-secondary, rgba(0, 0, 0, 0.65));
    border: 1px solid var(--border-color-hover, #d9d9d9);
  }
  .btn-cancel:hover { background: var(--bg-hover, rgba(0, 0, 0, 0.04)); }
  .btn-confirm {
    background: var(--color-danger, #dc2626);
    color: var(--text-on-primary, #fff);
  }
  .btn-confirm:hover { background: var(--color-danger-hover, #ef4444); }
`;

class CrapConfirm extends HTMLElement {
  constructor() {
    super();

    // Set to true between confirm-click and the resulting requestSubmit so
    // the re-fired submit event passes through without re-prompting.
    /** @type {boolean} */
    this._confirmed = false;

    /** @type {HTMLFormElement|null} */
    this._pendingForm = null;

    /** @type {boolean} */
    this._connected = false;

    const root = this.attachShadow({ mode: 'open' });
    root.adoptedStyleSheets = [sheet];

    /** @type {HTMLParagraphElement} */
    this._messageEl = h('p');
    /** @type {HTMLButtonElement} */
    this._cancelBtn = h('button', { class: 'btn-cancel', type: 'button', text: t('cancel') });
    /** @type {HTMLButtonElement} */
    this._confirmBtn = h('button', { class: 'btn-confirm', type: 'button', text: t('confirm') });
    /** @type {HTMLDialogElement} */
    this._dialog = h('dialog', null,
      h('div', { class: 'dialog__body' }, this._messageEl),
      h('div', { class: 'dialog__actions' }, this._cancelBtn, this._confirmBtn),
    );
    root.append(h('slot'), this._dialog);
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    // Capture phase so we run before HTMX's direct listener on the child
    // form. In the target phase, HTMX's handler fires first and sends the
    // request before we get a chance to intercept.
    this.addEventListener('submit', (e) => this._onSubmit(e), true);
    this._cancelBtn.addEventListener('click', () => this._onCancel());
    this._confirmBtn.addEventListener('click', () => this._onConfirm());
  }

  disconnectedCallback() {
    // Do NOT reset _connected — listeners on `this` and on shadow-DOM
    // elements survive DOM moves. Resetting causes duplicate submit
    // interception on reconnect, which blocks confirmed submissions.
  }

  /** @param {Event} e */
  _onSubmit(e) {
    if (this._confirmed) {
      this._confirmed = false;
      return; // let the confirmed re-submit through
    }
    e.preventDefault();
    e.stopImmediatePropagation();
    this._pendingForm = /** @type {HTMLFormElement} */ (e.target);
    this._messageEl.textContent = this.getAttribute('message') || t('are_you_sure');
    this._dialog.showModal();
  }

  _onCancel() {
    this._pendingForm = null;
    this._dialog.close();
  }

  _onConfirm() {
    this._dialog.close();
    const form = this._pendingForm;
    if (!form) return;
    this._pendingForm = null;
    this._confirmed = true;
    form.requestSubmit();
  }
}

customElements.define('crap-confirm', CrapConfirm);
