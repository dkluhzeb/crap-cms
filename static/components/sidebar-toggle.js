/**
 * Mobile sidebar toggle — `<crap-sidebar>`.
 *
 * Wraps the admin sidebar. Listens for clicks on
 * `[data-action="toggle-sidebar"]` (anywhere in the document) to open
 * / close, ESC to close, and any HTMX request to close (so navigating
 * dismisses the menu).
 *
 * @module sidebar-toggle
 */

const SIDEBAR_OPEN_CLASS = 'sidebar--open';
const BACKDROP_VISIBLE_CLASS = 'sidebar-backdrop--visible';

class CrapSidebar extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {((e: Event) => void)|null} */
    this._onDocClick = null;
    /** @type {((e: KeyboardEvent) => void)|null} */
    this._onEscape = null;
    /** @type {(() => void)|null} */
    this._onNav = null;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    this._onDocClick = (e) => this._onToggleClick(e);
    this._onEscape = (e) => {
      if (e.key === 'Escape' && this._isOpen()) this._close();
    };
    this._onNav = () => this._close();

    document.addEventListener('click', this._onDocClick);
    document.addEventListener('keydown', /** @type {EventListener} */ (this._onEscape));
    document.addEventListener('htmx:beforeRequest', this._onNav);
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    if (this._onDocClick) document.removeEventListener('click', this._onDocClick);
    if (this._onEscape) document.removeEventListener('keydown', /** @type {EventListener} */ (this._onEscape));
    if (this._onNav) document.removeEventListener('htmx:beforeRequest', this._onNav);
  }

  /** @param {Event} e */
  _onToggleClick(e) {
    if (!(e.target instanceof Element)) return;
    if (!e.target.closest('[data-action="toggle-sidebar"]')) return;

    const sidebar = this.querySelector('.sidebar');
    if (!sidebar) return;
    const opening = !sidebar.classList.contains(SIDEBAR_OPEN_CLASS);
    sidebar.classList.toggle(SIDEBAR_OPEN_CLASS, opening);
    this._backdrop()?.classList.toggle(BACKDROP_VISIBLE_CLASS, opening);
  }

  _close() {
    this.querySelector('.sidebar')?.classList.remove(SIDEBAR_OPEN_CLASS);
    this._backdrop()?.classList.remove(BACKDROP_VISIBLE_CLASS);
  }

  _isOpen() {
    return !!this.querySelector(`.${SIDEBAR_OPEN_CLASS}`);
  }

  /**
   * The backdrop lives outside this component (it overlays the page),
   * so we look it up on the document. It's optional — desktop layouts
   * have no backdrop.
   */
  _backdrop() {
    return document.querySelector('.sidebar-backdrop');
  }
}

customElements.define('crap-sidebar', CrapSidebar);
