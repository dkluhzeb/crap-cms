/**
 * <crap-drawer> — Slide-in drawer panel using native <dialog>.
 *
 * Uses Shadow DOM with CSS custom properties from :root for theming.
 * A single shared instance is created lazily and reused.
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
    this.shadowRoot.innerHTML = `
      <style>
        :host {
          display: contents;
        }
        dialog:not([open]) {
          display: none;
        }
        dialog {
          position: fixed;
          inset: 0 0 0 auto;
          width: min(540px, 95vw);
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
          padding: 1rem 1.25rem;
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
          width: 32px;
          height: 32px;
          border-radius: var(--radius-sm, 4px);
          cursor: pointer;
          color: var(--text-secondary, rgba(0, 0, 0, 0.65));
          font-size: 20px;
          font-family: 'Material Symbols Outlined';
        }
        .header__close:hover {
          background: var(--bg-hover, rgba(0, 0, 0, 0.04));
          color: var(--text-primary, rgba(0, 0, 0, 0.88));
        }
        .body {
          flex: 1;
          overflow-y: auto;
          padding: 1rem 1.25rem;
        }
      </style>
      <dialog>
        <div class="header">
          <h3 class="header__title"></h3>
          <button class="header__close" type="button" aria-label="Close">close</button>
        </div>
        <div class="body"></div>
      </dialog>
    `;

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
    this.shadowRoot.querySelector('.body').innerHTML = '';
    dialog.showModal();
  }

  /** Close the drawer. */
  close() {
    const dialog = this.shadowRoot.querySelector('dialog');
    dialog.close();
    this.shadowRoot.querySelector('.body').innerHTML = '';
  }

  /** @returns {HTMLElement} The body content container. */
  get body() {
    return this.shadowRoot.querySelector('.body');
  }
}

customElements.define('crap-drawer', CrapDrawer);

/* ── Shared singleton ────────────────────────────────────────── */

/** @type {CrapDrawer | null} */
let instance = null;

/**
 * Lazily create (or reuse) a shared <crap-drawer> element.
 * @returns {CrapDrawer}
 */
export function getDrawer() {
  if (!instance) {
    instance = /** @type {CrapDrawer} */ (document.createElement('crap-drawer'));
    document.body.appendChild(instance);
  }
  return instance;
}
