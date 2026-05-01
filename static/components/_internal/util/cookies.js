/**
 * Cookie helpers.
 *
 * Centralises the small read-this-cookie pattern that was previously
 * duplicated across 6+ component files (each with subtly different
 * regexes and return shapes).
 *
 * @module util/cookies
 * @stability internal
 */

/**
 * Read the value of a cookie by name.
 *
 * @param {string} name
 * @returns {string|null} Decoded value, or `null` if the cookie isn't set.
 */
export function readCookie(name) {
  // RFC 6265: cookies are separated by `; ` (a semicolon and a single
  // space). We allow `\s*` for tolerance against tab/newline insertions
  // by intermediaries.
  const m = document.cookie.match(new RegExp(`(?:^|;\\s*)${name}=([^;]*)`));
  if (!m) return null;
  try {
    return decodeURIComponent(m[1]);
  } catch {
    return m[1];
  }
}

/**
 * Read the CSRF cookie value (`crap_csrf`).
 *
 * Returns `''` (empty string) when missing — most callers send the value
 * as a header, where `''` is a no-op.
 *
 * @returns {string}
 */
export function readCsrfCookie() {
  return readCookie('crap_csrf') ?? '';
}

/**
 * Write a cookie. Centralises the one place we touch `document.cookie`
 * for writes so callers don't have to hand-roll the cookie string and
 * so the lint suppression for direct `document.cookie` access lives here
 * (callers can keep `noDocumentCookie` enabled).
 *
 * @param {string} name
 * @param {string} value
 * @param {object} [opts]
 * @param {string} [opts.path]      Cookie path (default `/`).
 * @param {number} [opts.maxAge]    Max-age in seconds (omit for session cookie).
 * @param {string} [opts.sameSite]  `Lax` (default), `Strict`, or `None`.
 * @param {boolean} [opts.secure]   Set the `Secure` flag.
 */
export function writeCookie(name, value, opts = {}) {
  const path = opts.path ?? '/';
  const sameSite = opts.sameSite ?? 'Lax';
  const parts = [`${name}=${value}`, `path=${path}`, `SameSite=${sameSite}`];
  if (opts.maxAge !== undefined) parts.push(`max-age=${opts.maxAge}`);
  if (opts.secure) parts.push('Secure');
  // biome-ignore lint/suspicious/noDocumentCookie: writeCookie is the single sanctioned cookie-write site
  document.cookie = parts.join(';');
}
