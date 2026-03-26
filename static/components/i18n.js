/**
 * i18n helper — reads translations from the `#crap-i18n` JSON data island.
 *
 * @module i18n
 */

/** @type {Record<string, string>} */
let translations = {};

/** @type {boolean} */
let loaded = false;

/**
 * Load translations from the data island (lazy, cached).
 * @returns {Record<string, string>}
 */
function load() {
  if (loaded) return translations;
  loaded = true;
  try {
    const el = document.getElementById('crap-i18n');
    if (el) translations = JSON.parse(el.textContent || '{}');
  } catch { /* fallback to empty */ }
  return translations;
}

/**
 * Get a translated string by key. Falls back to the key itself.
 * Supports `{{variable}}` interpolation via an optional params object.
 *
 * @param {string} key
 * @param {Record<string, string|number>} [params]
 * @returns {string}
 */
export function t(key, params) {
  const strings = load();
  let value = strings[key] || key;
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      value = value.replaceAll(`{{${k}}}`, String(v));
    }
  }
  return value;
}
