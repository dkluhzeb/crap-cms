/**
 * Locale-aware date formatting for <time> elements.
 *
 * Replaces the text content of <time datetime="..."> elements with a
 * locale-formatted date string using the browser's Intl.DateTimeFormat.
 */

/** @type {Intl.DateTimeFormat} */
const formatter = new Intl.DateTimeFormat(undefined, {
  year: 'numeric',
  month: 'short',
  day: 'numeric',
  hour: '2-digit',
  minute: '2-digit',
});

/**
 * Format all <time> elements with a datetime attribute.
 */
function formatTimeElements() {
  document.querySelectorAll('time[datetime]').forEach(
    /** @param {HTMLTimeElement} el */ (el) => {
      const raw = el.getAttribute('datetime');
      if (!raw) return;
      // SQLite datetime format: "YYYY-MM-DD HH:MM:SS" — add T for ISO parse
      const date = new Date(raw.replace(' ', 'T'));
      if (!isNaN(date.getTime())) {
        el.textContent = formatter.format(date);
      }
    }
  );
}

document.addEventListener('DOMContentLoaded', formatTimeElements);
document.addEventListener('htmx:afterSettle', formatTimeElements);
