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
      const index = /** @type {HTMLElement} */ (btn).dataset.tabIndex;
      if (index == null) return;

      this.querySelectorAll('.form__tabs-tab').forEach((t) => {
        t.classList.remove('form__tabs-tab--active');
        t.setAttribute('aria-selected', 'false');
      });
      this.querySelectorAll('.form__tabs-panel').forEach((p) => {
        p.classList.add('form__tabs-panel--hidden');
      });

      btn.classList.add('form__tabs-tab--active');
      btn.setAttribute('aria-selected', 'true');
      const panel = this.querySelector(`[data-tab-panel="${index}"]`);
      if (panel) panel.classList.remove('form__tabs-panel--hidden');
    });
  }
}

customElements.define('crap-tabs', CrapTabs);
