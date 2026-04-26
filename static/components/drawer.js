/**
 * Slide-in drawer panel — `<crap-drawer>`.
 *
 * Renders a native `<dialog>` slid in from the right edge with a
 * heading + body. The component is intended to be a singleton: any
 * page can find the connected instance by dispatching a
 * `crap:drawer-request` event whose `detail.instance` the drawer
 * fills in.
 *
 * @example
 * const evt = new CustomEvent('crap:drawer-request', { detail: {} });
 * document.dispatchEvent(evt);
 * const drawer = evt.detail.instance;
 * drawer?.open({ title: 'Browse Media' });
 * drawer?.body.append(myList);   // mount content into the drawer body
 * // ... user closes, or call `drawer.close()`.
 *
 * @module drawer
 */

import { css } from './css.js';
import { h, clear } from './h.js';
import { t } from './i18n.js';

const sheet = css`
  :host {
    display: contents;
  }
  dialog:not([open]) {
    display: none;
  }
  dialog {
    position: fixed;
    inset: 0 0 0 auto;
    width: min(33.75rem, 95vw);
    max-width: none;
    max-height: none;
    height: 100vh;
    margin: 0;
    border: none;
    padding: 0;
    background: var(--bg-elevated, #fff);
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
    box-shadow: var(--shadow-lg, 0 16px 48px rgba(0, 0, 0, 0.2));
    font-family: inherit;
    display: flex;
    flex-direction: column;
  }
  dialog::backdrop {
    background: rgba(0, 0, 0, 0.4);
    backdrop-filter: blur(2px);
  }
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-lg, 1rem) 1.25rem;
    border-bottom: 1px solid var(--border-color, #e5e7eb);
    flex-shrink: 0;
  }
  .header__title {
    font-size: var(--text-lg, 1.125rem);
    font-weight: 600;
    margin: 0;
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
  }
  .header__close {
    all: unset;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: var(--control-md, 2rem);
    height: var(--control-md, 2rem);
    border-radius: var(--radius-sm, 4px);
    cursor: pointer;
    color: var(--text-secondary, rgba(0, 0, 0, 0.65));
    font-size: var(--icon-md, 1.125rem);
    font-family: 'Material Symbols Outlined';
  }
  .header__close:hover {
    background: var(--bg-hover, rgba(0, 0, 0, 0.04));
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
  }
  .body {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-lg, 1rem) 1.25rem;
  }

  /* ── Form elements inside drawer (shadow DOM blocks global rules) ── */
  .body label {
    font-size: var(--text-sm, 0.8125rem);
    font-weight: 500;
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
  }
  .body input,
  .body select,
  .body textarea {
    width: 100%;
    box-sizing: border-box;
    border: 1px solid var(--input-border, rgba(0, 0, 0, 0.15));
    box-shadow: var(--shadow-sm, 0 1px 2px rgba(0, 0, 0, 0.04));
    border-radius: var(--radius-md, 6px);
    padding: var(--space-sm, 0.5rem) var(--space-md, 0.75rem);
    font-size: var(--text-base, 0.875rem);
    font-weight: 400;
    line-height: 1.5;
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
    background-color: var(--input-bg, #fff);
    transition: border-color 0.15s ease, box-shadow 0.15s ease;
  }
  .body input,
  .body select {
    height: var(--input-height, 36px);
  }
  .body select {
    appearance: none;
    padding-right: 2rem;
    background-image: var(--select-arrow);
    background-repeat: no-repeat;
    background-position: right 0.625rem center;
    background-size: 1rem;
  }
  .body input:focus,
  .body select:focus,
  .body textarea:focus {
    border-color: var(--color-primary, #1677ff);
    box-shadow: 0 0 0 2px var(--color-primary-bg, rgba(22, 119, 255, 0.06));
    outline: 0;
  }
  .body input::placeholder,
  .body textarea::placeholder {
    color: var(--text-tertiary, rgba(0, 0, 0, 0.45));
  }
  .body input[type="checkbox"],
  .body input[type="radio"] {
    width: auto;
    height: auto;
    padding: 0;
    border: none;
    box-shadow: none;
    accent-color: var(--color-primary, #1677ff);
  }

  /* ── Buttons inside drawer ── */
  .body .button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-sm, 0.5rem);
    height: var(--button-height, 2.25rem);
    padding: 0 var(--space-md, 0.75rem);
    border: 1px solid var(--border-color, rgba(0, 0, 0, 0.08));
    border-radius: var(--radius-md, 6px);
    background: var(--bg-elevated, #fff);
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
    font-size: var(--text-sm, 0.8125rem);
    font-weight: 500;
    cursor: pointer;
    text-decoration: none;
    transition: border-color 0.15s ease, background-color 0.15s ease;
    width: auto;
    box-shadow: none;
  }
  .body .button:hover {
    border-color: var(--color-primary, #1677ff);
    color: var(--color-primary, #1677ff);
  }
  .body .button--primary {
    background: var(--color-primary, #1677ff);
    border-color: var(--color-primary, #1677ff);
    color: var(--text-on-primary, #fff);
  }
  .body .button--primary:hover {
    background: var(--color-primary-hover, #4096ff);
    border-color: var(--color-primary-hover, #4096ff);
    color: var(--text-on-primary, #fff);
  }
  .body .button--ghost {
    background: transparent;
    border-color: transparent;
  }
  .body .button--ghost:hover {
    background: var(--bg-hover, rgba(0, 0, 0, 0.04));
    border-color: transparent;
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
  }
  .body .button--small {
    height: var(--button-height-sm, 1.75rem);
    padding: 0 var(--space-sm, 0.5rem);
    font-size: var(--text-xs, 0.75rem);
  }

  /* ── Column picker ── */
  .body .column-picker__list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2xs, 2px);
  }
  .body .column-picker__item {
    display: flex;
    align-items: center;
    gap: var(--space-sm, 0.5rem);
    padding: var(--space-xs, 0.25rem) var(--space-sm, 0.5rem);
    border-radius: var(--radius-sm, 4px);
    font-size: var(--text-sm, 0.8125rem);
    font-weight: 400;
    cursor: pointer;
    transition: background-color 0.15s ease;
  }
  .body .column-picker__item:hover {
    background: var(--bg-hover, rgba(0, 0, 0, 0.04));
  }
  .body .column-picker__footer {
    margin-top: var(--space-lg, 1rem);
    display: flex;
    justify-content: flex-end;
  }

  /* ── Filter builder ── */
  .body .filter-builder__rows {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm, 0.5rem);
    margin-bottom: var(--space-md, 0.75rem);
  }
  .body .filter-builder__row {
    display: flex;
    align-items: center;
    gap: var(--space-xs, 0.25rem);
  }
  .body .filter-builder__row select,
  .body .filter-builder__row input {
    width: auto;
    flex: 0 0 auto;
  }
  .body .filter-builder__field {
    min-width: 7.5rem;
  }
  .body .filter-builder__op {
    min-width: 5.625rem;
  }
  .body .filter-builder__value-wrap {
    flex: 1;
    min-width: 0;
  }
  .body .filter-builder__value-wrap select,
  .body .filter-builder__value-wrap input {
    width: 100%;
    flex: 1;
  }
  .body .filter-builder__remove {
    flex-shrink: 0;
  }
  .body .filter-builder__footer {
    margin-top: var(--space-lg, 1rem);
    display: flex;
    justify-content: space-between;
  }

  /* Material Symbols inside drawer */
  .body .material-symbols-outlined {
    font-family: 'Material Symbols Outlined';
    font-weight: normal;
    font-style: normal;
    font-size: var(--icon-md, 1.125rem);
    line-height: 1;
    letter-spacing: normal;
    text-transform: none;
    display: inline-block;
    white-space: nowrap;
    word-wrap: normal;
    direction: ltr;
    -webkit-font-smoothing: antialiased;
  }
`;

class CrapDrawer extends HTMLElement {
  constructor() {
    super();

    /** @type {boolean} */
    this._connected = false;
    /** @type {((e: Event) => void)|null} */
    this._handleRequest = null;

    const root = this.attachShadow({ mode: 'open' });
    root.adoptedStyleSheets = [sheet];

    /** @type {HTMLHeadingElement} */
    this._titleEl = h('h3', { class: 'header__title' });
    /** @type {HTMLDivElement} */
    this._bodyEl = h('div', { class: 'body' });
    /**
     * "close" is the Material Symbols glyph name; the font-family on
     * `.header__close` renders it as the X icon.
     * @type {HTMLButtonElement}
     */
    this._closeBtn = h('button', {
      class: 'header__close',
      type: 'button',
      'aria-label': t('close'),
      text: 'close',
    });
    /** @type {HTMLDialogElement} */
    this._dialog = h('dialog', null,
      h('div', { class: 'header' }, this._titleEl, this._closeBtn),
      this._bodyEl,
    );
    root.append(this._dialog);

    this._closeBtn.addEventListener('click', () => this.close());
    // Backdrop click closes (target === dialog only when the user clicks
    // the area outside the dialog's content rectangle).
    this._dialog.addEventListener('click', (e) => {
      if (e.target === this._dialog) this.close();
    });
    // ESC fires `cancel`; pre-empt the default close so we route through
    // our `close()` and clear the body.
    this._dialog.addEventListener('cancel', (e) => {
      e.preventDefault();
      this.close();
    });
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    this._handleRequest = (e) => {
      const detail = /** @type {CustomEvent} */ (e).detail;
      if (!detail.instance) detail.instance = this;
    };
    document.addEventListener('crap:drawer-request', this._handleRequest);
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    if (this._handleRequest) {
      document.removeEventListener('crap:drawer-request', this._handleRequest);
    }
  }

  /**
   * Open the drawer with `opts.title`. The body is cleared so the
   * caller mounts fresh content into `drawer.body` after this returns.
   *
   * @param {{ title: string }} opts
   */
  open(opts) {
    this._titleEl.textContent = opts.title || '';
    clear(this._bodyEl);
    this._dialog.showModal();
  }

  close() {
    this._dialog.close();
    clear(this._bodyEl);
  }

  /** Body content container — mount points for callers. */
  get body() {
    return this._bodyEl;
  }
}

customElements.define('crap-drawer', CrapDrawer);

