/**
 * Admin UI locale picker — `<crap-ui-locale-picker>`.
 *
 * Server-rendered toggle + dropdown of available admin UI locales. On
 * select, POSTs `/admin/api/locale` and reloads so the next render
 * comes back in the new language.
 *
 * Required slotted markup:
 *   - `[data-ui-locale-toggle]` — open/close button
 *   - `[data-ui-locale-dropdown]` — container of `[data-ui-locale-value="…"]` items
 *
 * @module ui-locale-picker
 * @stability stable
 */

import { CrapPickerBase } from './_internal/picker-base.js';
import { readCsrfCookie } from './_internal/util/cookies.js';

const LOCALE_ENDPOINT = '/admin/api/locale';

class CrapUiLocalePicker extends CrapPickerBase {
  static toggleSelector = '[data-ui-locale-toggle]';
  static dropdownSelector = '[data-ui-locale-dropdown]';
  static itemSelector = '[data-ui-locale-value]';
  static openClass = 'locale-picker__dropdown--open';
  static valueDatasetKey = 'uiLocaleValue';

  /** @param {string} locale */
  async _onValue(locale) {
    const csrf = readCsrfCookie();
    const body = new URLSearchParams({ locale });
    if (csrf) body.append('_csrf', csrf);

    try {
      const resp = await fetch(LOCALE_ENDPOINT, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/x-www-form-urlencoded',
          ...(csrf ? { 'X-CSRF-Token': csrf } : {}),
        },
        body,
      });
      if (resp.ok) location.reload();
    } catch {
      /* user can retry */
    }
  }
}

customElements.define('crap-ui-locale-picker', CrapUiLocalePicker);
