/**
 * Admin UI locale picker — saves preferred locale to server and reloads.
 *
 * Mirrors the theme picker pattern: toggle dropdown, select option, persist.
 * POSTs to /admin/api/locale, then reloads the page so the server renders
 * in the new language.
 */

import { registerInit } from './actions.js';

/**
 * Get the CSRF token from the crap_csrf cookie.
 * @returns {string|null}
 */
function getCsrf() {
  const m = document.cookie.match(/(?:^|; )crap_csrf=([^;]*)/);
  return m ? m[1] : null;
}

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

    dropdown.addEventListener('click', async (e) => {
      const btn = /** @type {HTMLElement} */ (e.target).closest('[data-locale-value]');
      if (!btn) return;
      const locale = /** @type {HTMLElement} */ (btn).dataset.localeValue;
      if (!locale) return;

      dropdown.classList.remove('locale-picker__dropdown--open');

      // POST to server
      const csrf = getCsrf();
      const body = new URLSearchParams({ locale });
      if (csrf) body.append('_csrf', csrf);

      try {
        const resp = await fetch('/admin/api/locale', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/x-www-form-urlencoded',
            ...(csrf ? { 'X-CSRF-Token': csrf } : {}),
          },
          body,
        });
        if (resp.ok) {
          location.reload();
        }
      } catch {
        // Silently fail — user can retry
      }
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
