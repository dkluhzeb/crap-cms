/**
 * Theme switching — `<crap-theme-picker>`.
 *
 * Provides theme persistence (localStorage), application (data-theme on <html>),
 * and a dropdown picker UI. Uses CSS custom properties from `:root` for theming.
 *
 * @namespace window.CrapTheme
 * @module theme
 */

window.CrapTheme = {
  /** @type {string} */
  _key: 'crap-theme',

  /**
   * @returns {string} Theme name or '' for default light.
   */
  get() {
    try {
      return localStorage.getItem(this._key) || '';
    } catch {
      return '';
    }
  },

  /**
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
   * @param {string} theme
   */
  set(theme) {
    try {
      if (theme) {
        localStorage.setItem(this._key, theme);
      } else {
        localStorage.removeItem(this._key);
      }
    } catch { /* storage unavailable */ }
    this.apply(theme);
  },
};

// Apply saved theme (also done via inline script in base.hbs for FOUC prevention)
window.CrapTheme.apply(window.CrapTheme.get());

class CrapThemePicker extends HTMLElement {
  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    const toggle = this.querySelector('[data-theme-toggle]');
    const dropdown = this.querySelector('[data-theme-dropdown]');
    if (!toggle || !dropdown) return;

    const updateActive = () => {
      const current = window.CrapTheme.get();
      dropdown.querySelectorAll('[data-theme-value]').forEach((btn) => {
        const val = /** @type {HTMLElement} */ (btn).dataset.themeValue;
        btn.classList.toggle('theme-picker__option--active', val === current);
      });
    };

    this._onToggle = (e) => {
      e.stopPropagation();
      dropdown.classList.toggle('theme-picker__dropdown--open');
      updateActive();
    };

    this._onSelect = (e) => {
      const btn = /** @type {HTMLElement} */ (e.target).closest('[data-theme-value]');
      if (!btn) return;
      window.CrapTheme.set(/** @type {HTMLElement} */ (btn).dataset.themeValue || '');
      dropdown.classList.remove('theme-picker__dropdown--open');
      updateActive();
    };

    this._onOutsideClick = (e) => {
      if (!this.contains(/** @type {Node} */ (e.target))) {
        dropdown.classList.remove('theme-picker__dropdown--open');
      }
    };

    toggle.addEventListener('click', this._onToggle);
    dropdown.addEventListener('click', this._onSelect);
    document.addEventListener('click', this._onOutsideClick);
  }

  disconnectedCallback() {
    const toggle = this.querySelector('[data-theme-toggle]');
    const dropdown = this.querySelector('[data-theme-dropdown]');
    if (toggle && this._onToggle) {
      toggle.removeEventListener('click', this._onToggle);
    }
    if (dropdown && this._onSelect) {
      dropdown.removeEventListener('click', this._onSelect);
    }
    if (this._onOutsideClick) {
      document.removeEventListener('click', this._onOutsideClick);
    }
  }
}

customElements.define('crap-theme-picker', CrapThemePicker);
