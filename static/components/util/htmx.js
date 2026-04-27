/**
 * HTMX event helpers.
 *
 * @module util/htmx
 */

/**
 * Extract the HTTP verb from an HTMX request event, normalised to
 * uppercase. Falls back to `''` if neither path is present.
 *
 * **Why both paths**: htmx 2 nests the request config under
 * `detail.requestConfig.verb` for `htmx:beforeRequest` and the
 * `xhr.config.verb` style events; htmx 1 used a flat `detail.verb` on
 * the same events (and some 2.x events still do). The dual lookup keeps
 * us correct against both shapes — do not collapse it to one path even
 * if the codebase appears to only need one. Removing the fallback would
 * silently break overlay scripts subscribing to legacy events through
 * an older htmx instance, and break us against any future event whose
 * detail shape differs again.
 *
 * @param {Event} e An HTMX `htmx:beforeRequest` / `htmx:configRequest` /
 *   `htmx:afterRequest` event.
 * @returns {string} `'GET'`, `'POST'`, etc., or `''`.
 */
export function getHttpVerb(e) {
  const detail = /** @type {any} */ (e).detail;
  return (detail?.requestConfig?.verb || detail?.verb || '').toUpperCase();
}
