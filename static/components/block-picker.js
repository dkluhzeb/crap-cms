/**
 * Block Picker — `<crap-block-picker>`.
 *
 * Enhances block-type select elements with optgroup grouping or visual
 * card picker. Dispatches `crap:request-add-block` CustomEvent to
 * request the parent `<crap-array-field>` add a row.
 *
 * Uses Shadow DOM for the visual picker grid. The select and add button
 * remain in light DOM (slotted) for form participation.
 *
 * @module block-picker
 */

import { h, clear } from './h.js';

class CrapBlockPicker extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    this.shadowRoot.adoptedStyleSheets = [sheet];
    this.shadowRoot.append(
      h('div', { class: 'picker' }),
      h('slot'),
    );

    const select = /** @type {HTMLSelectElement|null} */ (
      this.querySelector('.form__blocks-select')
    );
    if (!select) return;

    this._enhanceBlockSelect(select);

    // Handle add-block-row button click (dropdown mode)
    this.addEventListener('click', (e) => {
      const btn = /** @type {HTMLElement} */ (e.target).closest('[data-action="add-block-row"]');
      if (!btn) return;
      this.dispatchEvent(new CustomEvent('crap:request-add-block', {
        bubbles: true,
        detail: { templateId: /** @type {HTMLElement} */ (btn).dataset.templateId },
      }));
    });
  }

  /**
   * @param {HTMLSelectElement} select
   */
  _enhanceBlockSelect(select) {
    if (select.querySelector('optgroup')) return;

    const options = /** @type {HTMLOptionElement[]} */ ([...select.options]);
    const pickerMode = this.dataset.picker;

    if (pickerMode === 'card') {
      this._buildVisualPicker(select, options);
    } else if (options.some((o) => o.dataset.group)) {
      this._buildOptgroups(select, options);
    }
  }

  /**
   * @param {HTMLSelectElement} select
   * @param {HTMLOptionElement[]} options
   */
  _buildOptgroups(select, options) {
    /** @type {Map<string, HTMLOptionElement[]>} */
    const groups = new Map();
    /** @type {HTMLOptionElement[]} */
    const ungrouped = [];

    for (const opt of options) {
      const g = opt.dataset.group;
      if (g) {
        if (!groups.has(g)) groups.set(g, []);
        groups.get(g).push(opt);
      } else {
        ungrouped.push(opt);
      }
    }

    if (groups.size === 0) return;

    clear(select);
    for (const opt of ungrouped) select.appendChild(opt);
    for (const [name, opts] of groups) {
      const optgroup = document.createElement('optgroup');
      optgroup.label = name;
      for (const opt of opts) optgroup.appendChild(opt);
      select.appendChild(optgroup);
    }
  }

  /**
   * @param {HTMLSelectElement} select
   * @param {HTMLOptionElement[]} options
   */
  _buildVisualPicker(select, options) {
    const templateId = select.id.replace('block-type-', '');
    const pickerEl = this.shadowRoot.querySelector('.picker');

    pickerEl.className = 'picker picker--active';

    for (const opt of options) {
      const card = document.createElement('button');
      card.type = 'button';
      card.className = 'card';

      const imageUrl = opt.dataset.imageUrl;
      if (imageUrl) {
        const img = document.createElement('img');
        img.src = imageUrl;
        img.alt = opt.textContent || opt.value;
        img.className = 'card__img';
        card.appendChild(img);
      } else {
        const icon = document.createElement('span');
        icon.className = 'card__icon';
        icon.style.fontFamily = "'Material Symbols Outlined'";
        icon.textContent = 'widgets';
        card.appendChild(icon);
      }

      const label = document.createElement('span');
      label.className = 'card__label';
      label.textContent = opt.textContent || opt.value;
      card.appendChild(label);

      card.addEventListener('click', () => {
        select.value = opt.value;
        this.dispatchEvent(new CustomEvent('crap:request-add-block', {
          bubbles: true,
          detail: { templateId },
        }));
      });

      pickerEl.appendChild(card);
    }

    // Hide slotted select + button when visual picker is active
    this.shadowRoot.querySelector('slot').style.display = 'none';
  }

  static _styles() {
    return `
      :host {
        display: flex;
        align-items: center;
        gap: var(--space-md, 0.75rem);
        margin-top: var(--space-sm, 0.5rem);
      }

      ::slotted(select) {
        width: auto;
        min-width: calc(var(--base, 0.25rem) * 35);
      }

      .picker:not(.picker--active) {
        display: none;
      }

      .picker--active {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(calc(var(--base, 0.25rem) * 25), 1fr));
        gap: var(--space-sm, 0.5rem);
        width: 100%;
      }

      :host(:has(.picker--active)) {
        flex-wrap: wrap;
      }

      .card {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-xs, 0.25rem);
        padding: var(--space-sm, 0.5rem);
        border: 1px solid var(--border-default, var(--border-color, rgba(0, 0, 0, 0.08)));
        border-radius: var(--radius-md, 6px);
        background: var(--surface-primary, #fff);
        cursor: pointer;
        transition: border-color var(--transition-fast, 0.15s ease), box-shadow var(--transition-fast, 0.15s ease);
      }

      .card:hover {
        border-color: var(--accent-primary, var(--color-primary, #1677ff));
        box-shadow: var(--shadow-sm, 0 1px 2px rgba(0, 0, 0, 0.04));
      }

      .card__img {
        width: var(--icon-xl, 3rem);
        height: var(--icon-xl, 3rem);
        object-fit: contain;
      }

      .card__icon {
        font-size: var(--control-md, 2rem);
        color: var(--text-tertiary, rgba(0, 0, 0, 0.45));
      }

      .card__label {
        font-size: var(--text-xs, 0.75rem);
        color: var(--text-secondary, rgba(0, 0, 0, 0.65));
        text-align: center;
        line-height: 1.3;
      }
    `;
  }
}

const sheet = new CSSStyleSheet();
sheet.replaceSync(CrapBlockPicker._styles());

customElements.define('crap-block-picker', CrapBlockPicker);
