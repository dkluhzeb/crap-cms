/**
 * Tab field switching — `<crap-tabs>`.
 *
 * Handles tab switching via click delegation on `[data-action="switch-tab"]`.
 * State persistence is handled by `<crap-scroll-restore>`.
 *
 * @module tabs
 */

class CrapTabs extends HTMLElement {
  connectedCallback() {
    this.addEventListener('click', (e) => {
      const btn = /** @type {HTMLElement} */ (e.target).closest('[data-action="switch-tab"]');
      if (!btn) return;
      this._activateTab(btn);
    });

    this.addEventListener('keydown', (e) => {
      const target = /** @type {HTMLElement} */ (e.target);
      if (!target.matches('[data-action="switch-tab"]')) return;
      const tabs = /** @type {HTMLElement[]} */ (
        [...this.querySelectorAll('[data-action="switch-tab"]')]
      );
      if (tabs.length === 0) return;
      const currentIdx = tabs.indexOf(target);

      /** @type {HTMLElement|null} */
      let next = null;
      if (e.key === 'ArrowRight') {
        next = tabs[(currentIdx + 1) % tabs.length];
      } else if (e.key === 'ArrowLeft') {
        next = tabs[(currentIdx - 1 + tabs.length) % tabs.length];
      } else if (e.key === 'Home') {
        next = tabs[0];
      } else if (e.key === 'End') {
        next = tabs[tabs.length - 1];
      }

      if (next) {
        e.preventDefault();
        next.focus();
        this._activateTab(next);
      }
    });

    // Set initial tabindex on non-active tabs
    this._updateTabindex();
  }

  /** @param {HTMLElement} tab */
  _activateTab(tab) {
    const index = tab.dataset.tabIndex;
    if (index == null) return;

    this.querySelectorAll('.form__tabs-tab').forEach((t) => {
      t.classList.remove('form__tabs-tab--active');
      t.setAttribute('aria-selected', 'false');
    });
    this.querySelectorAll('.form__tabs-panel').forEach((p) => {
      p.classList.add('form__tabs-panel--hidden');
    });

    tab.classList.add('form__tabs-tab--active');
    tab.setAttribute('aria-selected', 'true');
    const panel = this.querySelector(`[data-tab-panel="${index}"]`);
    if (panel) panel.classList.remove('form__tabs-panel--hidden');

    this._updateTabindex();
  }

  _updateTabindex() {
    this.querySelectorAll('[data-action="switch-tab"]').forEach((tab) => {
      const isActive = tab.classList.contains('form__tabs-tab--active');
      tab.setAttribute('tabindex', isActive ? '0' : '-1');
    });
  }
}

customElements.define('crap-tabs', CrapTabs);
