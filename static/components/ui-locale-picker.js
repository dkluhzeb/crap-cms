/**
 * Admin UI locale picker — `<crap-ui-locale-picker>`.
 *
 * Server-rendered toggle + dropdown of available admin UI locales. On
 * select, POSTs `/admin/api/locale` and reloads so the next render
 * comes back in the new language.
 *
 * Required slotted markup:
 *   - `[data-ui-locale-toggle]` — open/close button
 *   - `[data-ui-locale-dropdown]` — container of `[data-ui-locale-value="…"]` items
 *
 * @module ui-locale-picker
 */

const LOCALE_ENDPOINT = '/admin/api/locale';

/** @returns {string} */
function readCsrfCookie() {
  const m = document.cookie.match(/(?:^|;\s*)crap_csrf=([^;]*)/);
  if (!m) return '';
  try { return decodeURIComponent(m[1]); } catch { return m[1]; }
}

class CrapUiLocalePicker extends HTMLElement {
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
    this._toggle = /** @type {HTMLElement|null} */ (this.querySelector('[data-ui-locale-toggle]'));
    this._dropdown = /** @type {HTMLElement|null} */ (this.querySelector('[data-ui-locale-dropdown]'));
    if (!this._toggle || !this._dropdown) return;
    this._connected = true;

    this._onToggle = (e) => {
      e.stopPropagation();
      this._dropdown?.classList.toggle('locale-picker__dropdown--open');
    };

    this._onSelect = (e) => {
      if (!(e.target instanceof Element)) return;
      const btn = /** @type {HTMLElement|null} */ (e.target.closest('[data-ui-locale-value]'));
      const locale = btn?.dataset.uiLocaleValue;
      if (!locale) return;
      this._dropdown?.classList.remove('locale-picker__dropdown--open');
      this._setLocale(locale);
    };

    this._onOutsideClick = (e) => {
      if (!(e.target instanceof Node)) return;
      if (!this.contains(e.target)) {
        this._dropdown?.classList.remove('locale-picker__dropdown--open');
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

  /**
   * POST the chosen locale and reload on success. Silent on failure —
   * the user can retry.
   *
   * @param {string} locale
   */
  async _setLocale(locale) {
    const csrf = readCsrfCookie();
    const body = new URLSearchParams({ locale });
    if (csrf) body.append('_csrf', csrf);

    try {
      const resp = await fetch(LOCALE_ENDPOINT, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/x-www-form-urlencoded',
          ...(csrf ? { 'X-CSRF-Token': csrf } : {}),
        },
        body,
      });
      if (resp.ok) location.reload();
    } catch { /* user can retry */ }
  }
}

customElements.define('crap-ui-locale-picker', CrapUiLocalePicker);
