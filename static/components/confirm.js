import { t } from './i18n.js';

/**
 * <crap-confirm> — Confirmation dialog that wraps destructive form actions.
 *
 * Intercepts `submit` events from child forms, shows a styled dialog,
 * and only allows submission through if the user confirms.
 *
 * For standalone HTMX buttons (not inside a child form), use `hx-confirm`
 * instead — the <crap-confirm-dialog> component handles those.
 *
 * @attr {string} message - Confirmation prompt text (default: "Are you sure?").
 *
 * @example
 * <crap-confirm message="Delete this item permanently?">
 *   <form method="post" action="/delete/123">
 *     <button type="submit" class="button button--danger">Delete</button>
 *   </form>
 * </crap-confirm>
 */
class CrapConfirm extends HTMLElement {
  constructor() {
    super();

    /**
     * Flag to bypass interception on confirmed re-submit.
     * @type {boolean}
     * @private
     */
    this._confirmed = false;

    /**
     * Reference to the form that triggered the confirmation.
     * @type {HTMLFormElement | null}
     * @private
     */
    this._pendingForm = null;

    /**
     * Guard against duplicate listener registration on reconnection.
     * @type {boolean}
     * @private
     */
    this._connected = false;

    this.attachShadow({ mode: 'open' });
    this.shadowRoot.innerHTML = `
      <style>
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
        dialog::backdrop {
          background: rgba(0, 0, 0, 0.4);
        }
        .dialog__body {
          padding: var(--space-xl, 1.5rem);
        }
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
          font-size: var(--text-base, 0.875rem);
          font-weight: 500;
          padding: var(--space-sm, 0.5rem) var(--space-lg, 1rem);
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
      </style>
      <slot></slot>
      <dialog>
        <div class="dialog__body">
          <p></p>
        </div>
        <div class="dialog__actions">
          <button class="btn-cancel" type="button">${t('cancel')}</button>
          <button class="btn-confirm" type="button">${t('confirm')}</button>
        </div>
      </dialog>
    `;
  }

  /** @returns {void} */
  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    /** @type {HTMLDialogElement} */
    const dialog = this.shadowRoot.querySelector('dialog');
    /** @type {HTMLParagraphElement} */
    const messageEl = this.shadowRoot.querySelector('.dialog__body p');
    /** @type {HTMLButtonElement} */
    const cancelBtn = this.shadowRoot.querySelector('.btn-cancel');
    /** @type {HTMLButtonElement} */
    const confirmBtn = this.shadowRoot.querySelector('.btn-confirm');

    // Use capture phase so this runs before HTMX's handler on the child
    // form. Without capture, HTMX's direct listener on the form fires
    // first (target phase) and sends the request before we can intercept.
    this.addEventListener('submit', (e) => {
      if (this._confirmed) {
        this._confirmed = false;
        return; // let re-submit through
      }
      e.preventDefault();
      e.stopImmediatePropagation();
      this._pendingForm = /** @type {HTMLFormElement} */ (e.target);
      messageEl.textContent = this.getAttribute('message') || 'Are you sure?';
      dialog.showModal();
    }, true);

    cancelBtn.addEventListener('click', () => {
      this._pendingForm = null;
      dialog.close();
    });

    confirmBtn.addEventListener('click', () => {
      dialog.close();
      if (this._pendingForm) {
        const form = this._pendingForm;
        this._pendingForm = null;
        this._confirmed = true;
        form.requestSubmit();
      }
    });
  }

  /** @returns {void} */
  disconnectedCallback() {
    this._connected = false;
  }
}

customElements.define('crap-confirm', CrapConfirm);
