/**
 * Tab field switching — `<crap-tabs>`.
 *
 * Click delegation on `[data-action="switch-tab"]` with full ARIA tab
 * keyboard navigation (Arrow keys, Home, End). State persistence
 * across navigation is handled by `<crap-scroll-restore>`.
 *
 * @module tabs
 */

const ACTIVE_TAB = 'form__tabs-tab--active';
const HIDDEN_PANEL = 'form__tabs-panel--hidden';
const TAB_SELECTOR = '[data-action="switch-tab"]';

class CrapTabs extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    this.addEventListener('click', (e) => this._onClick(e));
    this.addEventListener('keydown', (e) => this._onKeydown(e));
    this._updateTabindex();
  }

  /** @param {Event} e */
  _onClick(e) {
    if (!(e.target instanceof Element)) return;
    const btn = /** @type {HTMLElement|null} */ (e.target.closest(TAB_SELECTOR));
    if (btn) this._activateTab(btn);
  }

  /** @param {KeyboardEvent} e */
  _onKeydown(e) {
    if (!(e.target instanceof HTMLElement)) return;
    if (!e.target.matches(TAB_SELECTOR)) return;

    const tabs = /** @type {HTMLElement[]} */ ([...this.querySelectorAll(TAB_SELECTOR)]);
    if (tabs.length === 0) return;

    const next = this._neighborTab(e.key, tabs, tabs.indexOf(e.target));
    if (!next) return;
    e.preventDefault();
    next.focus();
    this._activateTab(next);
  }

  /**
   * Pick the keyboard target for arrow / Home / End navigation. Returns
   * `null` for unhandled keys.
   *
   * @param {string} key
   * @param {HTMLElement[]} tabs
   * @param {number} idx
   */
  _neighborTab(key, tabs, idx) {
    switch (key) {
      case 'ArrowRight':
        return tabs[(idx + 1) % tabs.length];
      case 'ArrowLeft':
        return tabs[(idx - 1 + tabs.length) % tabs.length];
      case 'Home':
        return tabs[0];
      case 'End':
        return tabs[tabs.length - 1];
      default:
        return null;
    }
  }

  /** @param {HTMLElement} tab */
  _activateTab(tab) {
    const index = tab.dataset.tabIndex;
    if (index == null) return;

    for (const t of this.querySelectorAll('.form__tabs-tab')) {
      t.classList.remove(ACTIVE_TAB);
      t.setAttribute('aria-selected', 'false');
    }
    for (const p of this.querySelectorAll('.form__tabs-panel')) {
      p.classList.add(HIDDEN_PANEL);
    }

    tab.classList.add(ACTIVE_TAB);
    tab.setAttribute('aria-selected', 'true');
    this.querySelector(`[data-tab-panel="${index}"]`)?.classList.remove(HIDDEN_PANEL);

    this._updateTabindex();
  }

  /**
   * Mark the active tab as `tabindex="0"` and the rest as `-1` so Tab
   * navigation enters the active tab and arrow keys move within the
   * group (standard ARIA tablist pattern).
   */
  _updateTabindex() {
    for (const tab of this.querySelectorAll(TAB_SELECTOR)) {
      tab.setAttribute('tabindex', tab.classList.contains(ACTIVE_TAB) ? '0' : '-1');
    }
  }
}

customElements.define('crap-tabs', CrapTabs);
