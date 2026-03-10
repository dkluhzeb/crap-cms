/**
 * Upload field preview — `<crap-upload-preview>`.
 *
 * Updates preview image and file info when the user selects a different
 * upload via the search widget or dropdown.
 *
 * @module uploads
 */

import { t } from './i18n.js';

class CrapUploadPreview extends HTMLElement {
  connectedCallback() {
    // Legacy: <select> for locale_locked fields
    const select = /** @type {HTMLSelectElement|null} */ (this.querySelector('[data-upload-select]'));
    if (select) {
      select.addEventListener('change', () => {
        const option = select.options[select.selectedIndex];
        if (!option || !option.value) {
          this._updatePreview(null, null, false);
          return;
        }
        this._updatePreview(
          option.getAttribute('data-thumbnail'),
          option.getAttribute('data-filename'),
          option.getAttribute('data-is-image') === 'true',
        );
      });
      return;
    }

    // Search widget: listen for crap:change events (bubbles from relationship-search)
    this.addEventListener('crap:change', () => {
      const hidden = /** @type {HTMLInputElement|null} */ (
        this.querySelector('.relationship-search__hidden input[type="hidden"]')
      );
      if (!hidden || !hidden.value) {
        this._updatePreview(null, null, false);
        return;
      }
      this._updatePreview(
        hidden.getAttribute('data-thumbnail'),
        hidden.getAttribute('data-filename'),
        hidden.getAttribute('data-is-image') === 'true',
      );
    });
  }

  /**
   * @param {string|null} thumbnailUrl
   * @param {string|null} filename
   * @param {boolean} isImage
   */
  _updatePreview(thumbnailUrl, filename, isImage) {
    const preview = /** @type {HTMLElement|null} */ (this.querySelector('.upload-field__preview'));
    const info = /** @type {HTMLElement|null} */ (this.querySelector('.upload-field__info'));

    if (preview) {
      if (thumbnailUrl && isImage) {
        preview.innerHTML = '<img src="' + thumbnailUrl + '" alt="' + t('preview') + '" />';
        preview.style.display = '';
      } else {
        preview.innerHTML = '';
        preview.style.display = 'none';
      }
    }

    if (info) {
      if (filename) {
        info.innerHTML =
          '<span class="material-symbols-outlined" style="font-size: 16px;">description</span>' +
          '<span class="upload-field__filename">' + filename + '</span>';
        info.style.display = '';
      } else {
        info.innerHTML = '';
        info.style.display = 'none';
      }
    }
  }
}

customElements.define('crap-upload-preview', CrapUploadPreview);
