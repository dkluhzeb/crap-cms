/**
 * CSRF helpers.
 *
 * Owns the two wire names — the `X-CSRF-Token` request header and the
 * `_csrf` form field — in one place, plus the header-merge dance that was
 * copy-pasted across every component that POSTs (`if (csrf) headers[...] =
 * csrf`). Callers get the token from {@link readCsrfCookie}; this module
 * decides how it rides on the request.
 *
 * @module util/csrf
 * @stability internal
 */

import { readCsrfCookie } from './cookies.js';

/** Request header carrying the CSRF token. */
export const CSRF_HEADER = 'X-CSRF-Token';

/** Form-body field name carrying the CSRF token. */
export const CSRF_FIELD = '_csrf';

/**
 * Merge the CSRF header into a headers object.
 *
 * Reads the cookie itself and adds `X-CSRF-Token` only when a token is
 * present (an absent token would send an empty header, which the server
 * treats as a failed check rather than a no-op). Returns a fresh object —
 * the `base` argument is never mutated.
 *
 * @param {Record<string, string>} [base]  Existing headers to extend (e.g. `Content-Type`).
 * @returns {Record<string, string>}
 */
export function csrfHeaders(base = {}) {
  const csrf = readCsrfCookie();
  return csrf ? { ...base, [CSRF_HEADER]: csrf } : { ...base };
}
