/**
 * Upload-field preview — `<crap-upload-preview>`.
 *
 * Refreshes the thumbnail + filename row when the user picks a
 * different upload. Two source shapes are supported:
 *
 *  - **Search widget** — the picked item lives on the hidden input
 *    inside a slotted `<crap-relationship-search>`. We listen for
 *    `crap:change` (bubbles from that component) and read the upload
 *    metadata from the hidden input's data attributes.
 *  - **Legacy `<select>`** — only used for locale-locked fields. The
 *    picked option carries the metadata directly.
 *
 * @module uploads
 */

import { h } from './h.js';
import { t } from './i18n.js';

/**
 * @typedef {{
 *   thumbnailUrl: string|null,
 *   filename: string|null,
 *   isImage: boolean,
 * }} UploadMeta
 */

const EMPTY_META = { thumbnailUrl: null, filename: null, isImage: false };

/**
 * Read upload metadata off any element that carries it as data
 * attributes (`<option>`, hidden `<input>`, etc.).
 *
 * @param {Element|null} el
 * @returns {UploadMeta}
 */
function readUploadMeta(el) {
  if (!el) return EMPTY_META;
  return {
    thumbnailUrl: el.getAttribute('data-thumbnail'),
    filename: el.getAttribute('data-filename'),
    isImage: el.getAttribute('data-is-image') === 'true',
  };
}

class CrapUploadPreview extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    // Legacy `<select>` path used for locale-locked fields. If present,
    // it owns the picker and the search-widget path doesn't apply.
    const select = /** @type {HTMLSelectElement|null} */ (this.querySelector('[data-upload-select]'));
    if (select) {
      this._setupSelect(select);
      return;
    }
    this._setupSearch();
  }

  /** @param {HTMLSelectElement} select */
  _setupSelect(select) {
    select.addEventListener('change', () => {
      const option = select.selectedOptions[0];
      this._updatePreview(option?.value ? readUploadMeta(option) : EMPTY_META);
    });
  }

  _setupSearch() {
    this.addEventListener('crap:change', () => {
      /** @type {HTMLInputElement|null} */
      const hidden = this.querySelector('crap-relationship-search input[type="hidden"]');
      this._updatePreview(hidden?.value ? readUploadMeta(hidden) : EMPTY_META);
    });
  }

  /** @param {UploadMeta} meta */
  _updatePreview(meta) {
    this._renderPreview(meta);
    this._renderInfo(meta);
  }

  /**
   * Render (or hide) the thumbnail image.
   * @param {UploadMeta} meta
   */
  _renderPreview(meta) {
    const preview = /** @type {HTMLElement|null} */ (this.querySelector('.upload-field__preview'));
    if (!preview) return;
    if (meta.thumbnailUrl && meta.isImage) {
      preview.replaceChildren(h('img', { src: meta.thumbnailUrl, alt: t('preview') }));
      preview.hidden = false;
    } else {
      preview.replaceChildren();
      preview.hidden = true;
    }
  }

  /**
   * Render (or hide) the icon + filename row.
   * @param {UploadMeta} meta
   */
  _renderInfo(meta) {
    const info = /** @type {HTMLElement|null} */ (this.querySelector('.upload-field__info'));
    if (!info) return;
    if (meta.filename) {
      info.replaceChildren(
        h('span', { class: ['material-symbols-outlined', 'icon--sm'], text: 'description' }),
        h('span', { class: 'upload-field__filename', text: meta.filename }),
      );
      info.hidden = false;
    } else {
      info.replaceChildren();
      info.hidden = true;
    }
  }
}

customElements.define('crap-upload-preview', CrapUploadPreview);
