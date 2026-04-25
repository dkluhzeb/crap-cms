import { h, clear } from './h.js';
import { t } from './i18n.js';

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
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
`);

/**
 * <crap-drawer> — Slide-in drawer panel using native <dialog>.
 *
 * Uses Shadow DOM with CSS custom properties from :root for theming.
 *
 * Instance-safe: each connected instance self-registers via
 * connectedCallback/disconnectedCallback. The getDrawer() helper
 * dispatches a synchronous event to find a connected instance.
 *
 * API:
 *   const drawer = getDrawer();
 *   drawer.open({ title: 'Browse Media' });
 *   drawer.body;        // content container element
 *   drawer.close();
 */
class CrapDrawer extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    this.shadowRoot.adoptedStyleSheets = [sheet];
    this.shadowRoot.append(
      h('dialog', null,
        h('div', { class: 'header' },
          h('h3', { class: 'header__title' }),
          h('button', {
            class: 'header__close',
            type: 'button',
            'aria-label': t('close'),
            text: 'close',
          }),
        ),
        h('div', { class: 'body' }),
      ),
    );

    const dialog = this.shadowRoot.querySelector('dialog');
    const closeBtn = this.shadowRoot.querySelector('.header__close');

    closeBtn.addEventListener('click', () => this.close());

    // Close on backdrop click
    dialog.addEventListener('click', (e) => {
      if (e.target === dialog) this.close();
    });

    // Close on Escape
    dialog.addEventListener('cancel', (e) => {
      e.preventDefault();
      this.close();
    });
  }

  /**
   * Open the drawer with a title.
   * @param {{ title: string }} opts
   */
  open(opts) {
    const dialog = this.shadowRoot.querySelector('dialog');
    this.shadowRoot.querySelector('.header__title').textContent = opts.title || '';
    clear(this.shadowRoot.querySelector('.body'));
    dialog.showModal();
  }

  /** Close the drawer. */
  close() {
    const dialog = this.shadowRoot.querySelector('dialog');
    dialog.close();
    clear(this.shadowRoot.querySelector('.body'));
  }

  /** @returns {HTMLElement} The body content container. */
  get body() {
    return this.shadowRoot.querySelector('.body');
  }

  /** @returns {void} */
  connectedCallback() {
    this._handleRequest = (e) => {
      if (!e.detail.instance) e.detail.instance = this;
    };
    document.addEventListener('crap:drawer-request', this._handleRequest);
  }

  /** @returns {void} */
  disconnectedCallback() {
    document.removeEventListener('crap:drawer-request', this._handleRequest);
  }
}

customElements.define('crap-drawer', CrapDrawer);

