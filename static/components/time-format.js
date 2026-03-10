/**
 * Locale-aware date formatting — `<crap-time>`.
 *
 * Replaces the text content with a locale-formatted date string
 * using the browser's Intl.DateTimeFormat. Reads the date from
 * the `datetime` attribute.
 *
 * @module time-format
 */

/** @type {Intl.DateTimeFormat} */
const formatter = new Intl.DateTimeFormat(undefined, {
  year: 'numeric',
  month: 'short',
  day: 'numeric',
  hour: '2-digit',
  minute: '2-digit',
});

class CrapTime extends HTMLElement {
  connectedCallback() {
    this._format();
  }

  static get observedAttributes() {
    return ['datetime'];
  }

  attributeChangedCallback() {
    if (this.isConnected) this._format();
  }

  _format() {
    const raw = this.getAttribute('datetime');
    if (!raw) return;
    // SQLite datetime format: "YYYY-MM-DD HH:MM:SS" — add T for ISO parse
    const date = new Date(raw.replace(' ', 'T'));
    if (!isNaN(date.getTime())) {
      this.textContent = formatter.format(date);
    }
  }
}

customElements.define('crap-time', CrapTime);
