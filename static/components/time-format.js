/**
 * Locale-aware date display — `<crap-time>`.
 *
 * Reads a timestamp from the `datetime` attribute and renders it as
 * locale-formatted text via `Intl.DateTimeFormat`. Re-renders when the
 * attribute changes (HTMX swap, programmatic `setAttribute`).
 *
 * @attr datetime  Either an ISO 8601 string or SQLite's
 *                 `"YYYY-MM-DD HH:MM:SS"` (we normalise the space to `T`
 *                 because Safari refuses to parse the space form).
 *
 * @module time-format
 */

const formatter = new Intl.DateTimeFormat(undefined, {
  year: 'numeric',
  month: 'short',
  day: 'numeric',
  hour: '2-digit',
  minute: '2-digit',
});

/**
 * Parse a datetime attribute value into a `Date`, or `null` if invalid.
 * Accepts ISO 8601 and SQLite-style `"YYYY-MM-DD HH:MM:SS"`.
 *
 * @param {string} raw
 * @returns {Date|null}
 */
function parseDatetime(raw) {
  // Safari rejects the SQLite space form; normalise to `T`.
  const date = new Date(raw.replace(' ', 'T'));
  return Number.isNaN(date.getTime()) ? null : date;
}

class CrapTime extends HTMLElement {
  static get observedAttributes() {
    return ['datetime'];
  }

  connectedCallback() {
    this._format();
  }

  attributeChangedCallback() {
    if (this.isConnected) this._format();
  }

  _format() {
    const raw = this.getAttribute('datetime');
    if (!raw) return;
    const date = parseDatetime(raw);
    if (date) this.textContent = formatter.format(date);
  }
}

customElements.define('crap-time', CrapTime);
