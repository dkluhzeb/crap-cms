/**
 * Admin UI locale picker — `<crap-ui-locale-picker>`.
 *
 * Server-rendered toggle + dropdown of available admin UI locales. On
 * select, POSTs `/admin/api/locale` and reloads so the next render
 * comes back in the new language. A refused or failed request is
 * toasted instead.
 *
 * Required slotted markup:
 *   - `[data-ui-locale-toggle]` — open/close button
 *   - `[data-ui-locale-dropdown]` — container of `[data-ui-locale-value="…"]` items
 *
 * @module ui-locale-picker
 * @category enhancer
 * @stability stable
 */

import { t } from './_internal/i18n.js';
import { CrapPickerBase } from './_internal/picker-base.js';
import { readCsrfCookie } from './_internal/util/cookies.js';
import { CSRF_FIELD, csrfHeaders } from './_internal/util/csrf.js';
import { toast, toastFailedResponse } from './_internal/util/toast.js';

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
    if (csrf) body.append(CSRF_FIELD, csrf);

    try {
      const resp = await fetch(LOCALE_ENDPOINT, {
        method: 'POST',
        headers: csrfHeaders({ 'Content-Type': 'application/x-www-form-urlencoded' }),
        body,
      });
      if (resp.ok) {
        location.reload();
        return;
      }
      toastFailedResponse(resp, t('request_failed'));
    } catch {
      // Network failure: say so — the user can retry.
      toast({ message: t('request_failed'), type: 'error' });
    }
  }
}

customElements.define('crap-ui-locale-picker', CrapUiLocalePicker);
