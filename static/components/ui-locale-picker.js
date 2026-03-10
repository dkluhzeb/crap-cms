/**
 * Admin UI locale picker — `<crap-ui-locale-picker>`.
 *
 * POSTs to /admin/api/locale, then reloads so the server renders
 * in the new language.
 *
 * @module ui-locale-picker
 */

/**
 * @returns {string|null}
 */
function getCsrf() {
  const m = document.cookie.match(/(?:^|; )crap_csrf=([^;]*)/);
  return m ? m[1] : null;
}

class CrapUiLocalePicker extends HTMLElement {
  connectedCallback() {
    const toggle = this.querySelector('[data-ui-locale-toggle]');
    const dropdown = this.querySelector('[data-ui-locale-dropdown]');
    if (!toggle || !dropdown) return;

    this._onToggle = (e) => {
      e.stopPropagation();
      dropdown.classList.toggle('locale-picker__dropdown--open');
    };

    this._onSelect = async (e) => {
      const btn = /** @type {HTMLElement} */ (e.target).closest('[data-ui-locale-value]');
      if (!btn) return;
      const locale = /** @type {HTMLElement} */ (btn).dataset.uiLocaleValue;
      if (!locale) return;

      dropdown.classList.remove('locale-picker__dropdown--open');

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
        if (resp.ok) location.reload();
      } catch {
        // Silently fail — user can retry
      }
    };

    this._onOutsideClick = (e) => {
      if (!this.contains(/** @type {Node} */ (e.target))) {
        dropdown.classList.remove('locale-picker__dropdown--open');
      }
    };

    toggle.addEventListener('click', this._onToggle);
    dropdown.addEventListener('click', this._onSelect);
    document.addEventListener('click', this._onOutsideClick);
  }

  disconnectedCallback() {
    if (this._onOutsideClick) {
      document.removeEventListener('click', this._onOutsideClick);
    }
  }
}

customElements.define('crap-ui-locale-picker', CrapUiLocalePicker);
