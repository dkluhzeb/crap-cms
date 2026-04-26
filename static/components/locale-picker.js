/**
 * Editor locale picker — `<crap-locale-picker>`.
 *
 * Server-rendered toggle button + dropdown of available locales. On
 * select, sets the `crap_editor_locale` cookie and full-reloads the
 * page so server-rendered field values switch to the new locale.
 *
 * Required slotted markup:
 *   - `[data-locale-toggle]` — open/close button
 *   - `[data-locale-dropdown]` — container of `[data-locale-value="…"]` items
 *
 * @module locale-picker
 */

import { CrapPickerBase } from './picker-base.js';
import { writeCookie } from './util/cookies.js';

/** Cookie lifetime for the editor-locale preference: 1 year. */
const LOCALE_COOKIE_MAX_AGE = 31536000;

class CrapLocalePicker extends CrapPickerBase {
  static toggleSelector = '[data-locale-toggle]';
  static dropdownSelector = '[data-locale-dropdown]';
  static itemSelector = '[data-locale-value]';
  static openClass = 'locale-picker__dropdown--open';
  static valueDatasetKey = 'localeValue';

  /** @param {string} locale */
  _onValue(locale) {
    writeCookie('crap_editor_locale', locale, { maxAge: LOCALE_COOKIE_MAX_AGE });
    location.reload();
  }
}

customElements.define('crap-locale-picker', CrapLocalePicker);
