/**
 * Tag Input Web Component — `<crap-tags>`.
 *
 * Provides a tag/chip-style input for has_many text and number fields.
 * Users type a value and press Enter (or comma for text) to add a tag.
 * Tags can be removed by clicking the X button or pressing Backspace
 * when the input is empty.
 *
 * Stores values as comma-separated in a hidden `<input>` for form submission.
 *
 * @module tags
 */

class CrapTags extends HTMLElement {
  constructor() {
    super();
    /** @type {HTMLInputElement|null} */
    this._hidden = null;
    /** @type {HTMLElement|null} */
    this._container = null;
    /** @type {HTMLInputElement|null} */
    this._input = null;
    /** @type {string} */
    this._fieldType = 'text';
    /** @type {boolean} */
    this._connected = false;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    this._hidden = this.querySelector('input[type="hidden"]');
    this._container = this.querySelector('.form__tags');
    this._input = this.querySelector('.form__tags-input');
    this._fieldType = this.dataset.fieldType || 'text';

    if (!this._hidden || !this._container || !this._input) return;

    // Bind events on existing remove buttons
    this._container.querySelectorAll('.form__tags-chip-remove').forEach((btn) => {
      btn.addEventListener('click', () => {
        const chip = btn.closest('.form__tags-chip');
        if (chip) {
          chip.remove();
          this._sync();
        }
      });
    });

    // Input: add tag on Enter, comma (text only), or Tab
    this._input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || (e.key === ',' && this._fieldType !== 'number')) {
        e.preventDefault();
        this._addFromInput();
      } else if (e.key === 'Backspace' && this._input.value === '') {
        // Remove last tag
        const chips = this._container.querySelectorAll('.form__tags-chip');
        if (chips.length > 0) {
          chips[chips.length - 1].remove();
          this._sync();
        }
      }
    });

    // Also add on blur (if there's pending text)
    this._input.addEventListener('blur', () => {
      this._addFromInput();
    });

    // Click on container focuses input
    this._container.addEventListener('click', (e) => {
      if (e.target === this._container) {
        this._input.focus();
      }
    });
  }

  disconnectedCallback() {
    // Do NOT reset _connected — listeners on `this` and child elements
    // survive DOM moves. Resetting causes duplicate handlers on reconnect.
  }

  /**
   * Read the input value, validate, and add as a tag.
   */
  _addFromInput() {
    if (!this._input) return;
    const raw = this._input.value.trim().replace(/,$/,'');
    if (!raw) return;

    if (this._fieldType === 'number') {
      const num = Number(raw);
      if (isNaN(num)) return;

      // Validate min/max
      const minAttr = this.dataset.min;
      const maxAttr = this.dataset.max;
      if (minAttr !== undefined && minAttr !== '' && num < Number(minAttr)) return;
      if (maxAttr !== undefined && maxAttr !== '' && num > Number(maxAttr)) return;
    } else {
      // Validate min/max length for text
      const minLen = this.dataset.minLength;
      const maxLen = this.dataset.maxLength;
      if (minLen && raw.length < Number(minLen)) return;
      if (maxLen && raw.length > Number(maxLen)) return;
    }

    // Prevent duplicates
    const existing = this._getValues();
    if (existing.includes(raw)) {
      this._input.value = '';
      return;
    }

    this._addChip(raw);
    this._input.value = '';
    this._sync();
  }

  /**
   * Create and insert a chip element.
   *
   * @param {string} value
   */
  _addChip(value) {
    const chip = document.createElement('span');
    chip.className = 'form__tags-chip';
    chip.dataset.value = value;
    chip.textContent = value;

    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'form__tags-chip-remove';
    btn.setAttribute('aria-label', 'Remove');
    btn.innerHTML = '&times;';
    btn.addEventListener('click', () => {
      chip.remove();
      this._sync();
    });

    chip.appendChild(btn);
    this._container.insertBefore(chip, this._input);
  }

  /**
   * Get all current tag values.
   *
   * @returns {string[]}
   */
  _getValues() {
    return [...this._container.querySelectorAll('.form__tags-chip')]
      .map((el) => /** @type {HTMLElement} */ (el).dataset.value || '');
  }

  /**
   * Sync the hidden input value from current chips.
   */
  _sync() {
    if (!this._hidden) return;
    this._hidden.value = this._getValues().join(',');
  }
}

customElements.define('crap-tags', CrapTags);
