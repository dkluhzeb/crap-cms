/**
 * Theme switcher — `<crap-theme-picker>` + `window.CrapTheme` namespace.
 *
 * Persists the selected theme to localStorage and applies it as
 * `<html data-theme="…">`. The picker is a dropdown that toggles open
 * on the slotted `[data-theme-toggle]` and selects via
 * `[data-theme-value]` items inside `[data-theme-dropdown]`.
 *
 * NOTE: `templates/layout/base.hbs` has an inline FOUC-prevention
 * script that reads `localStorage.getItem('crap-theme')` directly. If
 * the storage key changes here, change it there too.
 *
 * @module theme
 */

const STORAGE_KEY = 'crap-theme';

/**
 * Namespaced API used by inline templates and other components.
 *
 * @namespace
 */
window.CrapTheme = {
  /** @returns {string} Theme name or `''` for default light. */
  get() {
    try {
      return localStorage.getItem(STORAGE_KEY) || '';
    } catch {
      return '';
    }
  },

  /**
   * Apply `theme` to `<html data-theme>`. Empty string clears the attribute.
   * @param {string} theme
   */
  apply(theme) {
    if (theme) {
      document.documentElement.setAttribute('data-theme', theme);
    } else {
      document.documentElement.removeAttribute('data-theme');
    }
  },

  /**
   * Persist + apply.
   * @param {string} theme
   */
  set(theme) {
    try {
      if (theme) {
        localStorage.setItem(STORAGE_KEY, theme);
      } else {
        localStorage.removeItem(STORAGE_KEY);
      }
    } catch { /* storage unavailable */ }
    this.apply(theme);
  },
};

// Apply saved theme. Also done via the FOUC-prevention inline script in
// base.hbs — the JS-driven path keeps the apply current after HTMX swaps.
window.CrapTheme.apply(window.CrapTheme.get());

class CrapThemePicker extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {HTMLElement|null} */
    this._toggle = null;
    /** @type {HTMLElement|null} */
    this._dropdown = null;
    /** @type {((e: Event) => void)|null} */
    this._onToggle = null;
    /** @type {((e: Event) => void)|null} */
    this._onSelect = null;
    /** @type {((e: Event) => void)|null} */
    this._onOutsideClick = null;
  }

  connectedCallback() {
    if (this._connected) return;
    this._toggle = /** @type {HTMLElement|null} */ (this.querySelector('[data-theme-toggle]'));
    this._dropdown = /** @type {HTMLElement|null} */ (this.querySelector('[data-theme-dropdown]'));
    if (!this._toggle || !this._dropdown) return;
    this._connected = true;

    this._onToggle = (e) => {
      e.stopPropagation();
      this._dropdown?.classList.toggle('theme-picker__dropdown--open');
      this._refreshActive();
    };

    this._onSelect = (e) => {
      if (!(e.target instanceof Element)) return;
      const btn = /** @type {HTMLElement|null} */ (e.target.closest('[data-theme-value]'));
      if (!btn) return;
      window.CrapTheme.set(btn.dataset.themeValue || '');
      this._dropdown?.classList.remove('theme-picker__dropdown--open');
      this._refreshActive();
    };

    this._onOutsideClick = (e) => {
      if (!(e.target instanceof Node)) return;
      if (!this.contains(e.target)) {
        this._dropdown?.classList.remove('theme-picker__dropdown--open');
      }
    };

    this._toggle.addEventListener('click', this._onToggle);
    this._dropdown.addEventListener('click', this._onSelect);
    document.addEventListener('click', this._onOutsideClick);
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    if (this._toggle && this._onToggle) this._toggle.removeEventListener('click', this._onToggle);
    if (this._dropdown && this._onSelect) this._dropdown.removeEventListener('click', this._onSelect);
    if (this._onOutsideClick) document.removeEventListener('click', this._onOutsideClick);
    this._toggle = null;
    this._dropdown = null;
    this._onToggle = null;
    this._onSelect = null;
    this._onOutsideClick = null;
  }

  /** Mark the option matching the active theme. */
  _refreshActive() {
    if (!this._dropdown) return;
    const current = window.CrapTheme.get();
    for (const btn of /** @type {NodeListOf<HTMLElement>} */ (
      this._dropdown.querySelectorAll('[data-theme-value]')
    )) {
      btn.classList.toggle('theme-picker__option--active', btn.dataset.themeValue === current);
    }
  }
}

customElements.define('crap-theme-picker', CrapThemePicker);
