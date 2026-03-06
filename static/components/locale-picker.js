/**
 * Editor locale picker — sets crap_editor_locale cookie and reloads.
 */

import { registerInit } from './actions.js';

/**
 * Initialize locale picker UI. Safe to call multiple times (idempotent).
 */
function initLocalePicker() {
  document.querySelectorAll('[data-locale-picker]').forEach((picker) => {
    if (/** @type {HTMLElement} */ (picker).dataset.localeInit) return;
    /** @type {HTMLElement} */ (picker).dataset.localeInit = '1';

    const toggle = picker.querySelector('[data-locale-toggle]');
    const dropdown = picker.querySelector('[data-locale-dropdown]');
    if (!toggle || !dropdown) return;

    toggle.addEventListener('click', (e) => {
      e.stopPropagation();
      dropdown.classList.toggle('locale-picker__dropdown--open');
    });

    dropdown.addEventListener('click', (e) => {
      const btn = /** @type {HTMLElement} */ (e.target).closest('[data-locale-value]');
      if (!btn) return;
      const locale = /** @type {HTMLElement} */ (btn).dataset.localeValue;
      if (!locale) return;
      document.cookie = `crap_editor_locale=${locale};path=/;max-age=31536000;SameSite=Lax`;
      location.reload();
    });

    // Close on outside click
    document.addEventListener('click', (e) => {
      if (!picker.contains(/** @type {Node} */ (e.target))) {
        dropdown.classList.remove('locale-picker__dropdown--open');
      }
    });
  });
}

registerInit(initLocalePicker);
