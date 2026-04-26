/**
 * Tag input — `<crap-tags>`.
 *
 * Chip-style input for `has_many` text and number fields. Users type
 * a value and press Enter (or comma for text) to add a tag; chips can
 * be removed by clicking their `×` or pressing Backspace when the
 * input is empty.
 *
 * Shadow DOM holds the visual UI. The slotted `<input type="hidden">`
 * remains in light DOM (form participation), invisible (no slot
 * rendered for it).
 *
 * @attr data-field-type      `"text"` (default) | `"number"`.
 * @attr data-placeholder     Visible-input placeholder.
 * @attr data-min / data-max  Numeric bounds (number mode).
 * @attr data-min-length / data-max-length  Length bounds (text mode).
 * @attr data-readonly        Boolean — disables editing.
 * @attr data-error           Server-rendered marker for the error class.
 *
 * @example
 * <crap-tags data-field-type="text">
 *   <input type="hidden" name="tags" value="a,b,c" />
 * </crap-tags>
 *
 * @module tags
 */

import { css } from './css.js';
import { h } from './h.js';

const sheet = css`
  :host { display: block; }

  .tags {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-xs, 0.25rem);
    padding: var(--space-xs, 0.25rem) var(--space-sm, 0.5rem);
    border: 1px solid var(--border-default, var(--border-color, rgba(0, 0, 0, 0.08)));
    border-radius: var(--radius-md, 6px);
    background: var(--surface-primary, #fff);
    min-height: var(--input-height, 2.25rem);
    cursor: text;
  }
  .tags:focus-within {
    border-color: var(--accent-primary, var(--color-primary, #1677ff));
    box-shadow: 0 0 0 2px var(--accent-primary-bg, rgba(59, 130, 246, 0.1));
  }
  .tags--error { border-color: var(--color-danger, #ff4d4f); }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: var(--space-xs, 0.25rem);
    padding: var(--space-xs, 0.25rem) var(--space-sm, 0.5rem);
    background: var(--color-primary-bg, rgba(22, 119, 255, 0.06));
    border: 1px solid color-mix(in srgb, var(--color-primary, #1677ff) 20%, transparent);
    border-radius: var(--radius-md, 6px);
    font-size: var(--text-sm, 0.8125rem);
    font-weight: 500;
    line-height: 1.4;
    white-space: nowrap;
  }
  .chip__remove {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--text-tertiary, rgba(0, 0, 0, 0.45));
    cursor: pointer;
    font-size: var(--icon-sm, 1rem);
    line-height: 1;
    padding: 0;
    margin-left: var(--space-2xs, 2px);
    border-radius: var(--radius-sm, 4px);
    transition: color 0.15s ease, background 0.15s ease;
  }
  .chip__remove:hover {
    color: var(--color-danger, #ff4d4f);
    background: var(--color-danger-bg, rgba(255, 77, 79, 0.06));
  }

  .tags__input {
    flex: 1 1 calc(var(--base, 0.25rem) * 20);
    min-width: calc(var(--base, 0.25rem) * 20);
    height: auto;
    border: none;
    outline: none;
    background: transparent;
    box-shadow: none;
    font-size: var(--text-sm, 0.8125rem);
    font-family: inherit;
    padding: var(--space-xs, 0.25rem) 0;
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
  }
  .tags__input:focus {
    border: none;
    box-shadow: none;
  }
  .tags__input::placeholder {
    color: var(--text-tertiary, rgba(0, 0, 0, 0.45));
  }
  .chip + .tags__input { margin-left: var(--space-xs, 0.25rem); }
`;

class CrapTags extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {string[]} */
    this._values = [];
    /** @type {string} */
    this._fieldType = 'text';
    /** @type {boolean} */
    this._readonly = false;
    /** @type {HTMLDivElement|null} */
    this._container = null;
    /** @type {HTMLInputElement|null} */
    this._input = null;
    /** @type {HTMLInputElement|null} */
    this._hidden = null;

    this.attachShadow({ mode: 'open' });
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    this._readConfig();
    this._buildShadow();
    this._renderChips();
    if (!this._readonly) this._wireEvents();
  }

  /* ── Init ───────────────────────────────────────────────────── */

  _readConfig() {
    this._fieldType = this.dataset.fieldType || 'text';
    this._readonly = this.dataset.readonly !== undefined;
    this._hidden = /** @type {HTMLInputElement|null} */ (
      this.querySelector('input[type="hidden"]')
    );
    if (this._hidden?.value) {
      this._values = this._hidden.value.split(',').filter(Boolean);
    }
  }

  _buildShadow() {
    const root = /** @type {ShadowRoot} */ (this.shadowRoot);
    root.adoptedStyleSheets = [sheet];

    const isNumber = this._fieldType === 'number';
    this._input = h('input', {
      class: 'tags__input',
      type: isNumber ? 'number' : 'text',
      placeholder: this.dataset.placeholder || '',
      hidden: this._readonly,
      minlength: this.dataset.minLength,
      maxlength: this.dataset.maxLength,
      min: isNumber ? this.dataset.min : undefined,
      max: isNumber ? this.dataset.max : undefined,
    });

    const hasError = !!this.querySelector('.form__tags--error');
    this._container = h(
      'div',
      {
        class: ['tags', hasError && 'tags--error'],
        id: 'container',
      },
      this._input,
    );

    root.append(this._container);
  }

  /* ── Event wiring ───────────────────────────────────────────── */

  _wireEvents() {
    if (!this._input || !this._container) return;
    this._input.addEventListener('keydown', (e) => this._onKeydown(e));
    this._input.addEventListener('blur', () => this._addFromInput());
    this._container.addEventListener('click', (e) => {
      if (e.target === this._container) this._input?.focus();
    });
  }

  /** @param {KeyboardEvent} e */
  _onKeydown(e) {
    if (!this._input) return;
    const isComma = e.key === ',' && this._fieldType !== 'number';
    if (e.key === 'Enter' || isComma) {
      e.preventDefault();
      this._addFromInput();
      return;
    }
    if (e.key === 'Backspace' && this._input.value === '' && this._values.length > 0) {
      this._values.pop();
      this._sync();
      this._renderChips();
    }
  }

  /* ── Tag operations ─────────────────────────────────────────── */

  _addFromInput() {
    if (!this._input) return;
    const raw = this._input.value.trim().replace(/,$/, '');
    if (!raw || !this._isValidValue(raw) || this._values.includes(raw)) {
      this._input.value = '';
      return;
    }
    this._values.push(raw);
    this._input.value = '';
    this._sync();
    this._renderChips();
  }

  /**
   * Whether `raw` passes type-specific bounds (number range, text length).
   *
   * @param {string} raw
   */
  _isValidValue(raw) {
    if (this._fieldType === 'number') {
      const num = Number(raw);
      if (Number.isNaN(num)) return false;
      const min = this.dataset.min;
      const max = this.dataset.max;
      if (min && num < Number(min)) return false;
      if (max && num > Number(max)) return false;
      return true;
    }
    const minLen = this.dataset.minLength;
    const maxLen = this.dataset.maxLength;
    if (minLen && raw.length < Number(minLen)) return false;
    if (maxLen && raw.length > Number(maxLen)) return false;
    return true;
  }

  _renderChips() {
    if (!this._container || !this._input) return;
    for (const c of this._container.querySelectorAll('.chip')) c.remove();
    for (const value of this._values) {
      this._container.insertBefore(this._buildChip(value), this._input);
    }
  }

  /** @param {string} value */
  _buildChip(value) {
    return h(
      'span',
      { class: 'chip', dataset: { value } },
      value,
      !this._readonly &&
        h('button', {
          type: 'button',
          class: 'chip__remove',
          'aria-label': 'Remove',
          text: '×',
          onClick: () => this._removeValue(value),
        }),
    );
  }

  /** @param {string} value */
  _removeValue(value) {
    this._values = this._values.filter((v) => v !== value);
    this._sync();
    this._renderChips();
  }

  /** Push the joined values back to the form-bound hidden input. */
  _sync() {
    if (!this._hidden) return;
    this._hidden.value = this._values.join(',');
    this.dispatchEvent(new Event('crap:change', { bubbles: true }));
  }
}

customElements.define('crap-tags', CrapTags);
