/**
 * Mobile sidebar toggle — `<crap-sidebar>`.
 *
 * Manages sidebar open/close state in response to toggle button clicks,
 * Escape key, and HTMX navigation.
 *
 * @module sidebar-toggle
 */

class CrapSidebar extends HTMLElement {
  connectedCallback() {
    this._onDocClick = (e) => {
      const action = /** @type {HTMLElement} */ (e.target).closest('[data-action="toggle-sidebar"]');
      if (!action) return;
      const sidebar = this.querySelector('.sidebar');
      const backdrop = document.querySelector('.sidebar-backdrop');
      if (!sidebar) return;
      const opening = !sidebar.classList.contains('sidebar--open');
      sidebar.classList.toggle('sidebar--open', opening);
      backdrop?.classList.toggle('sidebar-backdrop--visible', opening);
    };

    this._onEscape = (e) => {
      if (e.key === 'Escape') this._close();
    };

    this._onNav = () => this._close();

    document.addEventListener('click', this._onDocClick);
    document.addEventListener('keydown', this._onEscape);
    document.addEventListener('htmx:beforeRequest', this._onNav);
  }

  disconnectedCallback() {
    document.removeEventListener('click', this._onDocClick);
    document.removeEventListener('keydown', this._onEscape);
    document.removeEventListener('htmx:beforeRequest', this._onNav);
  }

  _close() {
    this.querySelector('.sidebar')?.classList.remove('sidebar--open');
    document.querySelector('.sidebar-backdrop')?.classList.remove('sidebar-backdrop--visible');
  }
}

customElements.define('crap-sidebar', CrapSidebar);
