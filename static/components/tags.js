/**
 * Tag Input Web Component — `<crap-tags>`.
 *
 * Provides a tag/chip-style input for has_many text and number fields.
 * Users type a value and press Enter (or comma for text) to add a tag.
 * Tags can be removed by clicking the X button or pressing Backspace
 * when the input is empty.
 *
 * Uses Shadow DOM for the visual UI. The hidden `<input>` stays in light
 * DOM for form participation.
 *
 * @module tags
 */

class CrapTags extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    /** @type {string[]} */
    this._values = [];
    /** @type {string} */
    this._fieldType = 'text';
    /** @type {boolean} */
    this._connected = false;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    this._fieldType = this.dataset.fieldType || 'text';
    /** @type {boolean} */
    this._readonly = this.dataset.readonly !== undefined;

    // Read initial values from the hidden input
    const hidden = /** @type {HTMLInputElement|null} */ (
      this.querySelector('input[type="hidden"]')
    );
    if (hidden && hidden.value) {
      this._values = hidden.value.split(',').filter(Boolean);
    }

    // Check for error state from initial template
    const hasError = !!this.querySelector('.form__tags--error');

    // Clear light DOM except the hidden input
    const lightChildren = [...this.children];
    for (const child of lightChildren) {
      if (child !== hidden) child.remove();
    }

    // Build shadow UI
    this.shadowRoot.adoptedStyleSheets = [sheet];
    this.shadowRoot.innerHTML = `
      <div class="tags${hasError ? ' tags--error' : ''}" id="container">
        <input
          class="tags__input"
          type="${this._fieldType === 'number' ? 'number' : 'text'}"
          placeholder="${this.dataset.placeholder || ''}"
          ${this.dataset.minLength ? `minlength="${this.dataset.minLength}"` : ''}
          ${this.dataset.maxLength ? `maxlength="${this.dataset.maxLength}"` : ''}
          ${this._fieldType === 'number' && this.dataset.min ? `min="${this.dataset.min}"` : ''}
          ${this._fieldType === 'number' && this.dataset.max ? `max="${this.dataset.max}"` : ''}
        />
      </div>
    `;

    this._container = this.shadowRoot.getElementById('container');
    this._input = /** @type {HTMLInputElement} */ (
      this.shadowRoot.querySelector('.tags__input')
    );
    this._hidden = hidden;

    this._renderChips();

    if (this._readonly) {
      this._input.style.display = 'none';
      return;
    }

    // Input: add tag on Enter, comma (text only)
    this._input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || (e.key === ',' && this._fieldType !== 'number')) {
        e.preventDefault();
        this._addFromInput();
      } else if (e.key === 'Backspace' && this._input.value === '' && this._values.length > 0) {
        this._values.pop();
        this._sync();
        this._renderChips();
      }
    });

    // Add on blur
    this._input.addEventListener('blur', () => this._addFromInput());

    // Click on container focuses input
    this._container.addEventListener('click', (e) => {
      if (/** @type {HTMLElement} */ (e.target) === this._container) {
        this._input.focus();
      }
    });
  }

  _addFromInput() {
    if (!this._input) return;
    const raw = this._input.value.trim().replace(/,$/, '');
    if (!raw) return;

    if (this._fieldType === 'number') {
      const num = Number(raw);
      if (isNaN(num)) return;

      const minAttr = this.dataset.min;
      const maxAttr = this.dataset.max;
      if (minAttr !== undefined && minAttr !== '' && num < Number(minAttr)) return;
      if (maxAttr !== undefined && maxAttr !== '' && num > Number(maxAttr)) return;
    } else {
      const minLen = this.dataset.minLength;
      const maxLen = this.dataset.maxLength;
      if (minLen && raw.length < Number(minLen)) return;
      if (maxLen && raw.length > Number(maxLen)) return;
    }

    // Prevent duplicates
    if (this._values.includes(raw)) {
      this._input.value = '';
      return;
    }

    this._values.push(raw);
    this._input.value = '';
    this._sync();
    this._renderChips();
  }

  _renderChips() {
    // Remove existing chips
    this._container.querySelectorAll('.chip').forEach((c) => c.remove());

    // Insert chips before input
    for (const value of this._values) {
      const chip = document.createElement('span');
      chip.className = 'chip';
      chip.dataset.value = value;
      chip.textContent = value;

      if (!this._readonly) {
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'chip__remove';
        btn.setAttribute('aria-label', 'Remove');
        btn.innerHTML = '&times;';
        btn.addEventListener('click', () => {
          this._values = this._values.filter((v) => v !== value);
          this._sync();
          this._renderChips();
        });
        chip.appendChild(btn);
      }
      this._container.insertBefore(chip, this._input);
    }
  }

  _sync() {
    if (!this._hidden) return;
    this._hidden.value = this._values.join(',');
    this.dispatchEvent(new Event('crap:change', { bubbles: true }));
  }

  static _styles() {
    return `
      :host {
        display: block;
      }

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

      .tags--error {
        border-color: var(--color-danger, #ff4d4f);
      }

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

      .chip + .tags__input {
        margin-left: var(--space-xs, 0.25rem);
      }
    `;
  }
}

const sheet = new CSSStyleSheet();
sheet.replaceSync(CrapTags._styles());

customElements.define('crap-tags', CrapTags);
