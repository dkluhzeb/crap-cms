/**
 * Theme switcher — `<crap-theme-picker>` + theme state API.
 *
 * Persists the selected theme to localStorage and applies it as
 * `<html data-theme="…">`. The picker is a dropdown that toggles open
 * on the slotted `[data-theme-toggle]` and selects via
 * `[data-theme-value]` items inside `[data-theme-dropdown]`.
 *
 * The state API (`get` / `set` / `apply`) is exposed via the
 * `window.crap.theme` namespace (see `global.js`). Other code should
 * import from this module directly, or use `window.crap.theme`.
 *
 * NOTE: `templates/layout/base.hbs` has an inline FOUC-prevention
 * script that reads `localStorage.getItem('crap-theme')` directly. If
 * the storage key changes here, change it there too.
 *
 * @module theme
 * @stability stable
 */

import { CrapPickerBase } from './_internal/picker-base.js';

const STORAGE_KEY = 'crap-theme';

/** @returns {string} Theme name or `''` for default light. */
export function getTheme() {
  try {
    return localStorage.getItem(STORAGE_KEY) || '';
  } catch {
    return '';
  }
}

/**
 * Apply `theme` to `<html data-theme>`. Empty string clears the attribute.
 * @param {string} theme
 */
export function applyTheme(theme) {
  if (theme) {
    document.documentElement.setAttribute('data-theme', theme);
  } else {
    document.documentElement.removeAttribute('data-theme');
  }
}

/**
 * Persist + apply.
 * @param {string} theme
 */
export function setTheme(theme) {
  try {
    if (theme) {
      localStorage.setItem(STORAGE_KEY, theme);
    } else {
      localStorage.removeItem(STORAGE_KEY);
    }
  } catch {
    /* storage unavailable */
  }
  applyTheme(theme);
}

// Apply saved theme. Also done via the FOUC-prevention inline script in
// base.hbs — the JS-driven path keeps the apply current after HTMX swaps.
applyTheme(getTheme());

class CrapThemePicker extends CrapPickerBase {
  static toggleSelector = '[data-theme-toggle]';
  static dropdownSelector = '[data-theme-dropdown]';
  static itemSelector = '[data-theme-value]';
  static openClass = 'theme-picker__dropdown--open';
  static valueDatasetKey = 'themeValue';

  /** @param {string} theme */
  _onValue(theme) {
    setTheme(theme || '');
    this._refreshActive();
  }

  _afterToggle() {
    this._refreshActive();
  }

  /** Mark the option matching the active theme. */
  _refreshActive() {
    if (!this._dropdown) return;
    const current = getTheme();
    for (const btn of /** @type {NodeListOf<HTMLElement>} */ (
      this._dropdown.querySelectorAll('[data-theme-value]')
    )) {
      btn.classList.toggle('theme-picker__option--active', btn.dataset.themeValue === current);
    }
  }
}

customElements.define('crap-theme-picker', CrapThemePicker);
