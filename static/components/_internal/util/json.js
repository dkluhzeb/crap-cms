/**
 * JSON parsing helpers for component config sourced from the DOM.
 *
 * @module util/json
 * @stability internal
 */

/**
 * Parse a JSON-encoded HTML attribute, returning `fallback` when the
 * attribute is missing or malformed.
 *
 * @template T
 * @param {Element} el
 * @param {string} attr
 * @param {T} fallback
 * @returns {T}
 */
export function parseJsonAttribute(el, attr, fallback) {
  const raw = el.getAttribute(attr);
  if (!raw) return fallback;
  try {
    return JSON.parse(raw);
  } catch {
    return fallback;
  }
}

/**
 * Read a server-rendered JSON data island (`<script type="application/json"
 * id="…">…</script>` or any element whose `textContent` is JSON).
 *
 * Tries scoped lookup first (in case the island is inside `host`), then
 * falls back to a document-wide `getElementById`.
 *
 * @template T
 * @param {Element} host
 * @param {string} id
 * @param {T} fallback
 * @returns {T}
 */
export function readDataIsland(host, id, fallback) {
  const el = host.querySelector(`#${id}`) || document.getElementById(id);
  if (!el) return fallback;
  try {
    return JSON.parse(el.textContent || '');
  } catch {
    return fallback;
  }
}
