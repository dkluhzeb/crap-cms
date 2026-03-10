/**
 * <crap-confirm-dialog> — Standalone confirmation dialog for HTMX actions.
 *
 * Replaces the native `window.confirm()` used by `hx-confirm` with a styled
 * dialog. All styles are encapsulated in Shadow DOM. CSS custom properties
 * from :root pierce the shadow boundary for theming support.
 *
 * Instance-safe: each connected instance registers its own htmx:confirm
 * listener via connectedCallback/disconnectedCallback.
 *
 * Usage: Add `hx-confirm="Are you sure?"` to any HTMX-powered element.
 */
import { t } from './i18n.js';

class CrapConfirmDialog extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    this.shadowRoot.innerHTML = `
      <style>
        :host {
          display: contents;
        }
        dialog {
          border: none;
          border-radius: 12px;
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
        .body {
          padding: 1.5rem;
        }
        .body p {
          margin: 0;
          font-size: var(--text-sm, 0.875rem);
          color: var(--text-primary, rgba(0, 0, 0, 0.88));
          line-height: 1.5;
        }
        .actions {
          display: flex;
          justify-content: flex-end;
          gap: var(--space-sm, 0.5rem);
          padding: 0 1.5rem 1.5rem;
        }
        button {
          font-family: inherit;
          font-size: var(--text-sm, 0.875rem);
          font-weight: 500;
          padding: 0.5rem 1rem;
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
        .cancel:hover {
          background: var(--bg-hover, rgba(0, 0, 0, 0.04));
        }
        .confirm {
          background: var(--color-danger, #dc2626);
          color: var(--text-on-primary, #fff);
        }
        .confirm:hover {
          background: var(--color-danger-hover, #ef4444);
        }
      </style>
      <dialog>
        <div class="body">
          <p></p>
        </div>
        <div class="actions">
          <button class="cancel" type="button">${t('cancel')}</button>
          <button class="confirm" type="button">${t('confirm')}</button>
        </div>
      </dialog>
    `;
  }

  /**
   * Show the confirmation dialog with a message.
   * Returns a Promise that resolves to true (confirm) or false (cancel).
   *
   * @param {string} message - The confirmation prompt text.
   * @param {{ confirmLabel?: string, cancelLabel?: string }} [opts]
   * @returns {Promise<boolean>}
   */
  prompt(message, opts = {}) {
    const { confirmLabel = t('confirm'), cancelLabel = t('cancel') } = opts;
    return new Promise((resolve) => {
      const dialog = this.shadowRoot.querySelector('dialog');
      this.shadowRoot.querySelector('p').textContent = message;

      const cancelBtn = this.shadowRoot.querySelector('.cancel');
      const confirmBtn = this.shadowRoot.querySelector('.confirm');
      cancelBtn.textContent = cancelLabel;
      confirmBtn.textContent = confirmLabel;

      const cleanup = () => {
        cancelBtn.removeEventListener('click', onCancel);
        confirmBtn.removeEventListener('click', onConfirm);
      };

      const onCancel = () => {
        dialog.close();
        cleanup();
        resolve(false);
      };

      const onConfirm = () => {
        dialog.close();
        cleanup();
        resolve(true);
      };

      cancelBtn.addEventListener('click', onCancel);
      confirmBtn.addEventListener('click', onConfirm);

      dialog.showModal();
    });
  }

  /** @returns {void} */
  connectedCallback() {
    // Respond to getConfirmDialog() requests
    this._handleRequest = (e) => {
      if (!e.detail.instance) e.detail.instance = this;
    };
    document.addEventListener('crap:confirm-dialog-request', this._handleRequest);

    // Intercept HTMX's native confirm() and show a styled dialog instead
    this._handleHtmxConfirm = (evt) => {
      if (evt._crapHandled) return;
      const question = evt.detail.question;
      if (!question) return;

      evt._crapHandled = true;
      evt.preventDefault();

      this.prompt(question).then((confirmed) => {
        if (confirmed) evt.detail.issueRequest();
      });
    };
    document.addEventListener('htmx:confirm', this._handleHtmxConfirm);
  }

  /** @returns {void} */
  disconnectedCallback() {
    document.removeEventListener('crap:confirm-dialog-request', this._handleRequest);
    document.removeEventListener('htmx:confirm', this._handleHtmxConfirm);
  }
}

customElements.define('crap-confirm-dialog', CrapConfirmDialog);

/* ── Public API ──────────────────────────────────────────────── */

/**
 * Get a connected <crap-confirm-dialog> instance.
 * Uses a synchronous CustomEvent so each instance self-registers.
 * @returns {CrapConfirmDialog | null}
 */
export function getConfirmDialog() {
  const evt = new CustomEvent('crap:confirm-dialog-request', { detail: {} });
  document.dispatchEvent(evt);
  return /** @type {CrapConfirmDialog | null} */ (evt.detail.instance || null);
}
