/**
 * i18n helper — reads translations from the `#crap-i18n` JSON data
 * island that the server renders into every admin page.
 *
 * Translations are loaded lazily on the first `t()` call and cached.
 * The cache is invalidated on HTMX body swaps so a server-driven
 * locale change picks up new strings without a full page reload.
 *
 * @module i18n
 * @stability internal
 */

/** ID of the JSON data island the server renders the translations into. */
const DATA_ISLAND_ID = 'crap-i18n';

/** @type {Record<string, string>} */
let translations = {};

/** @type {boolean} */
let loaded = false;

/**
 * Read the data island and populate the cache. Calls after the first
 * are no-ops until {@link invalidate} flips `loaded` back.
 */
function load() {
  if (loaded) return;
  loaded = true;
  const el = document.getElementById(DATA_ISLAND_ID);
  if (!el) return;
  try {
    translations = JSON.parse(el.textContent || '{}');
  } catch (err) {
    // Page still works (every t() call falls back to the raw key) but
    // shipping malformed JSON in the data island means every label is
    // shown verbatim. Surface loudly so it's caught in dev.
    console.error('crap-i18n: failed to parse translations data island', err);
  }
}

/**
 * Mark the cache as stale so the next `t()` call re-reads the data
 * island. Existing translations stay until then so an in-flight render
 * doesn't see an empty map.
 */
function invalidate() {
  loaded = false;
}

document.addEventListener(
  'htmx:afterSettle',
  /** @param {Event} e */ (e) => {
    const detail = /** @type {CustomEvent} */ (e).detail;
    if (detail.target === document.body) invalidate();
  },
);

/**
 * Look up a translated string by key. Falls back to the key itself if
 * missing. Supports `{{name}}` placeholders interpolated from `params`.
 *
 * @param {string} key
 * @param {Record<string, string|number>} [params]
 * @returns {string}
 */
export function t(key, params) {
  load();
  let value = translations[key] || key;
  if (!params) return value;
  for (const [k, v] of Object.entries(params)) {
    value = value.replaceAll(`{{${k}}}`, String(v));
  }
  return value;
}
