/**
 * Toast notifications — `<crap-toast>`.
 *
 * Renders fixed-position toast messages with type-based colouring and
 * auto-dismiss. Two ways to show one:
 *  - **Programmatic**: dispatch `crap:toast` with
 *    `{ message, type?, duration? }` in `detail`.
 *  - **HTMX response header**: `X-Crap-Toast` carrying either a plain
 *    string message or a JSON `{ "message": "…", "type": "…" }`.
 *
 * @example
 * <crap-toast></crap-toast>
 *
 * @example
 * document.dispatchEvent(new CustomEvent('crap:toast', {
 *   detail: { message: 'Item created', type: 'success' },
 * }));
 *
 * @example HTTP
 * X-Crap-Toast: {"message": "Saved", "type": "success"}
 *
 * @module toast
 */

import { css } from './css.js';
import { h } from './h.js';

/** Default auto-dismiss delay in ms. Pass `0` to {@link CrapToast.show} to keep open. */
const DEFAULT_DURATION_MS = 3000;

const sheet = css`
  :host {
    position: fixed;
    bottom: var(--space-xl, 1.5rem);
    right: var(--space-xl, 1.5rem);
    z-index: 10000;
    display: flex;
    flex-direction: column;
    gap: var(--space-sm, 0.5rem);
    pointer-events: none;
  }
  .toast {
    display: flex;
    align-items: center;
    gap: var(--space-sm, 0.5rem);
    padding: var(--space-md, 0.75rem) 1.25rem;
    border-radius: var(--radius-lg, 8px);
    font-family: inherit;
    font-size: var(--text-base, 0.875rem);
    font-weight: 500;
    color: var(--text-on-primary, #fff);
    background: var(--bg-elevated, #1f2937);
    box-shadow: var(--shadow-lg, 0 8px 24px rgba(0, 0, 0, 0.15));
    pointer-events: auto;
    cursor: pointer;
    animation: toast-in 0.3s ease forwards;
    max-width: 23.75rem;
  }
  .toast.removing {
    animation: toast-out 0.25s ease forwards;
  }
  .toast--success { background: var(--color-success, #16a34a); }
  .toast--error   { background: var(--color-danger, #dc2626); }
  .toast--info    { background: var(--color-primary, #1677ff); }
  @keyframes toast-in {
    from { opacity: 0; transform: translateY(12px) scale(0.96); }
    to   { opacity: 1; transform: translateY(0) scale(1); }
  }
  @keyframes toast-out {
    from { opacity: 1; transform: translateY(0) scale(1); }
    to   { opacity: 0; transform: translateY(-8px) scale(0.96); }
  }
`;

/**
 * @typedef {'success' | 'error' | 'info'} ToastType
 *
 * @typedef {{ message: string, type?: ToastType, duration?: number }} ToastDetail
 */

class CrapToast extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {((e: Event) => void)|null} */
    this._onToastRequest = null;
    /** @type {((e: Event) => void)|null} */
    this._onAfterRequest = null;

    const root = this.attachShadow({ mode: 'open' });
    root.adoptedStyleSheets = [sheet];
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    this._onToastRequest = (e) => {
      const detail = /** @type {CustomEvent<ToastDetail & { _handled?: boolean }>} */ (e).detail;
      if (detail._handled) return;
      detail._handled = true;
      this.show(detail.message, detail.type, detail.duration);
    };
    this._onAfterRequest = (e) => this._handleHtmxResponse(e);

    document.addEventListener('crap:toast', this._onToastRequest);
    document.body.addEventListener('htmx:afterRequest', this._onAfterRequest);
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    if (this._onToastRequest) document.removeEventListener('crap:toast', this._onToastRequest);
    if (this._onAfterRequest) document.body.removeEventListener('htmx:afterRequest', this._onAfterRequest);
    this._onToastRequest = null;
    this._onAfterRequest = null;
  }

  /**
   * Show a toast.
   *
   * @param {string} message
   * @param {ToastType} [type='info']
   * @param {number} [duration=DEFAULT_DURATION_MS] Auto-dismiss delay in ms.
   *   Use `0` for a persistent toast (only dismissed by click).
   */
  show(message, type = 'info', duration = DEFAULT_DURATION_MS) {
    const toast = h('div', { class: ['toast', `toast--${type}`], text: message });
    /** @type {ShadowRoot} */ (this.shadowRoot).appendChild(toast);

    const remove = () => {
      toast.classList.add('removing');
      toast.addEventListener('animationend', () => toast.remove(), { once: true });
    };
    if (duration > 0) setTimeout(remove, duration);
    toast.addEventListener('click', remove);
  }

  /**
   * Inspect an HTMX `htmx:afterRequest` event for the `X-Crap-Toast`
   * response header and toast it. Status ≥ 400 picks the `error`
   * fallback; anything else picks `success`.
   *
   * @param {Event} e
   */
  _handleHtmxResponse(e) {
    const detail = /** @type {any} */ (e).detail;
    if (detail._crapToastHandled) return;
    /** @type {XMLHttpRequest|null} */
    const xhr = detail.xhr;
    if (!xhr) return;
    const header = xhr.getResponseHeader('X-Crap-Toast');
    if (!header) return;
    detail._crapToastHandled = true;

    /** @type {ToastType} */
    const fallbackType = xhr.status >= 400 ? 'error' : 'success';
    try {
      /** @type {{ message: string, type?: ToastType }} */
      const data = JSON.parse(header);
      this.show(data.message, data.type || fallbackType);
    } catch {
      this.show(header, fallbackType);
    }
  }
}

customElements.define('crap-toast', CrapToast);
