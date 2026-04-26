/**
 * HTMX event helpers.
 *
 * @module util/htmx
 */

/**
 * Extract the HTTP verb from an HTMX request event, normalised to
 * uppercase. HTMX sometimes carries the verb on `requestConfig.verb`
 * (newer paths) and sometimes on `detail.verb` (older paths). Falls
 * back to `''` if neither is present.
 *
 * @param {Event} e An HTMX `htmx:beforeRequest` / `htmx:configRequest` /
 *   `htmx:afterRequest` event.
 * @returns {string} `'GET'`, `'POST'`, etc., or `''`.
 */
export function getHttpVerb(e) {
  const detail = /** @type {any} */ (e).detail;
  return (detail?.requestConfig?.verb || detail?.verb || '').toUpperCase();
}
