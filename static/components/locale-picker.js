/**
 * Editor locale picker — `<crap-locale-picker>`.
 *
 * Sets crap_editor_locale cookie and reloads the page.
 *
 * @module locale-picker
 */

class CrapLocalePicker extends HTMLElement {
  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    const toggle = this.querySelector('[data-locale-toggle]');
    const dropdown = this.querySelector('[data-locale-dropdown]');
    if (!toggle || !dropdown) return;

    this._onToggle = (e) => {
      e.stopPropagation();
      dropdown.classList.toggle('locale-picker__dropdown--open');
    };

    this._onSelect = (e) => {
      const btn = /** @type {HTMLElement} */ (e.target).closest('[data-locale-value]');
      if (!btn) return;
      const locale = /** @type {HTMLElement} */ (btn).dataset.localeValue;
      if (!locale) return;
      document.cookie = `crap_editor_locale=${locale};path=/;max-age=31536000;SameSite=Lax`;
      location.reload();
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
    const toggle = this.querySelector('[data-locale-toggle]');
    const dropdown = this.querySelector('[data-locale-dropdown]');
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

customElements.define('crap-locale-picker', CrapLocalePicker);
