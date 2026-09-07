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
 * @attr format    How the value was stored — a `picker_appearance` name:
 *                 `dayAndTime` (default; local date + time), `dayOnly`
 *                 (calendar date, no clock, UTC), `monthOnly` (month + year,
 *                 UTC), `timeOnly` (shown as stored).
 *
 * @module time-format
 * @category enhancer
 * @stability stable
 */

const DATETIME = new Intl.DateTimeFormat(undefined, {
  year: 'numeric',
  month: 'short',
  day: 'numeric',
  hour: '2-digit',
  minute: '2-digit',
});

// Day-only and month-only values are stored as UTC calendar values (a date
// at UTC noon, a `YYYY-MM` month); formatting them in the viewer's zone would
// shift the day for UTC+12 and beyond, so they are rendered in UTC with no
// clock time.
const DAY = new Intl.DateTimeFormat(undefined, {
  year: 'numeric',
  month: 'short',
  day: 'numeric',
  timeZone: 'UTC',
});

const MONTH = new Intl.DateTimeFormat(undefined, {
  year: 'numeric',
  month: 'long',
  timeZone: 'UTC',
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
    return ['datetime', 'format'];
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
    const format = this.getAttribute('format') || 'dayAndTime';
    if (format === 'timeOnly') {
      this.textContent = raw;
      return;
    }
    const date = parseDatetime(raw);
    if (!date) return;
    const formatter = format === 'dayOnly' ? DAY : format === 'monthOnly' ? MONTH : DATETIME;
    this.textContent = formatter.format(date);
  }
}

customElements.define('crap-time', CrapTime);
