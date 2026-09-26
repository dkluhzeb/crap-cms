/**
 * Editor locale picker — `<crap-locale-picker>`.
 *
 * Server-rendered toggle button + dropdown of available locales. On
 * select — after the edit form's unsaved-changes question, when it has
 * any — sets the `crap_editor_locale` cookie and full-reloads the page so
 * server-rendered field values switch to the new locale.
 *
 * Required slotted markup:
 *   - `[data-locale-toggle]` — open/close button
 *   - `[data-locale-dropdown]` — container of `[data-locale-value="…"]` items
 *
 * @module locale-picker
 * @category enhancer
 * @stability stable
 */

import { confirmLeave } from './_internal/leave.js';
import { CrapPickerBase } from './_internal/picker-base.js';
import { writeCookie } from './_internal/util/cookies.js';

/** Cookie lifetime for the editor-locale preference: 1 year. */
const LOCALE_COOKIE_MAX_AGE = 31536000;

class CrapLocalePicker extends CrapPickerBase {
  static toggleSelector = '[data-locale-toggle]';
  static dropdownSelector = '[data-locale-dropdown]';
  static itemSelector = '[data-locale-value]';
  static openClass = 'locale-picker__dropdown--open';
  static valueDatasetKey = 'localeValue';

  /**
   * Switch the editor locale. An edit form with unsaved changes asks first,
   * and the cookie is written only once the editor chose to leave: written
   * before the question, a "Stay" would leave the old-locale form on screen
   * with the cookie already pointing at the new locale, and the save would
   * redirect into the other locale as if it had gone missing.
   *
   * @param {string} locale
   */
  async _onValue(locale) {
    if (!(await confirmLeave())) return;

    writeCookie('crap_editor_locale', locale, { maxAge: LOCALE_COOKIE_MAX_AGE });
    location.reload();
  }
}

customElements.define('crap-locale-picker', CrapLocalePicker);
