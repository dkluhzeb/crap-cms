/**
 * Editor locale picker — `<crap-locale-picker>`.
 *
 * Server-rendered toggle button + dropdown of available locales. On
 * select, sets the `crap_editor_locale` cookie and full-reloads the
 * page so server-rendered field values switch to the new locale.
 *
 * Required slotted markup:
 *   - `[data-locale-toggle]` — open/close button
 *   - `[data-locale-dropdown]` — container of `[data-locale-value="…"]` items
 *
 * @module locale-picker
 */

/** Cookie lifetime for the editor-locale preference: 1 year. */
const LOCALE_COOKIE_MAX_AGE = 31536000;

/**
 * Persist the chosen editor locale and reload so the server re-renders
 * field values in that locale.
 *
 * @param {string} locale
 */
function setEditorLocale(locale) {
  document.cookie =
    `crap_editor_locale=${locale};path=/;max-age=${LOCALE_COOKIE_MAX_AGE};SameSite=Lax`;
  location.reload();
}

class CrapLocalePicker extends HTMLElement {
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
    this._toggle = /** @type {HTMLElement|null} */ (this.querySelector('[data-locale-toggle]'));
    this._dropdown = /** @type {HTMLElement|null} */ (this.querySelector('[data-locale-dropdown]'));
    if (!this._toggle || !this._dropdown) return;
    this._connected = true;

    this._onToggle = (e) => {
      e.stopPropagation();
      this._dropdown?.classList.toggle('locale-picker__dropdown--open');
    };

    this._onSelect = (e) => {
      if (!(e.target instanceof Element)) return;
      const btn = /** @type {HTMLElement|null} */ (e.target.closest('[data-locale-value]'));
      const locale = btn?.dataset.localeValue;
      if (!locale) return;
      setEditorLocale(locale);
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
}

customElements.define('crap-locale-picker', CrapLocalePicker);
