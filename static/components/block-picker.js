/**
 * Block picker — `<crap-block-picker>`.
 *
 * Enhances a block-type `<select>` with one of two presentations:
 *
 *  - **dropdown mode** (default) — when options carry `data-group`, the
 *    flat option list is reorganised into `<optgroup>`s. The slotted
 *    select + add button stay visible and form-participating.
 *  - **card mode** — set `data-picker="card"` on the host. A grid of
 *    visual cards is rendered into the shadow root; clicking one sets
 *    the slotted select's value and dispatches
 *    `crap:request-add-block` so the parent `<crap-array-field>` adds
 *    a row. The slotted select + button are hidden via host-attribute CSS.
 *
 * @module block-picker
 */

import { css } from './css.js';
import { h } from './h.js';

const sheet = css`
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

  .picker:not(.picker--active) { display: none; }

  .picker--active {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(calc(var(--base, 0.25rem) * 25), 1fr));
    gap: var(--space-sm, 0.5rem);
    width: 100%;
  }

  :host(:has(.picker--active)) { flex-wrap: wrap; }

  /* Card mode hides the slotted dropdown + add button entirely. */
  :host([data-picker="card"]) slot { display: none; }

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
    transition:
      border-color var(--transition-fast, 0.15s ease),
      box-shadow var(--transition-fast, 0.15s ease);
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
    font-family: 'Material Symbols Outlined';
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

class CrapBlockPicker extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {HTMLElement|null} */
    this._pickerEl = null;
    this.attachShadow({ mode: 'open' });
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    const root = /** @type {ShadowRoot} */ (this.shadowRoot);
    root.adoptedStyleSheets = [sheet];
    this._pickerEl = h('div', { class: 'picker' });
    root.append(this._pickerEl, h('slot'));

    const select = /** @type {HTMLSelectElement|null} */ (
      this.querySelector('.form__blocks-select')
    );
    if (!select) return;

    this._enhanceBlockSelect(select);

    // Dropdown mode: the slotted "add" button bubbles a click here.
    this.addEventListener('click', (e) => {
      const btn = /** @type {HTMLElement|null} */ (
        /** @type {HTMLElement} */ (e.target).closest('[data-action="add-block-row"]')
      );
      if (!btn) return;
      this._requestAddBlock(btn.dataset.templateId || '');
    });
  }

  /** @param {HTMLSelectElement} select */
  _enhanceBlockSelect(select) {
    if (select.querySelector('optgroup')) return;

    const options = [...select.options];
    if (this.dataset.picker === 'card') {
      this._buildVisualPicker(select, options);
    } else if (options.some((o) => o.dataset.group)) {
      this._buildOptgroups(select, options);
    }
  }

  /**
   * Reorganise a flat list of options into `<optgroup>` elements based on
   * each option's `data-group`. Ungrouped options stay at the top.
   *
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
        const bucket = groups.get(g) ?? groups.set(g, []).get(g);
        /** @type {HTMLOptionElement[]} */ (bucket).push(opt);
      } else {
        ungrouped.push(opt);
      }
    }

    if (groups.size === 0) return;

    select.replaceChildren(
      ...ungrouped,
      ...[...groups].map(([label, opts]) => h('optgroup', { label }, ...opts)),
    );
  }

  /**
   * Render a card grid for each option. Clicking a card selects the
   * matching option and dispatches `crap:request-add-block`.
   *
   * @param {HTMLSelectElement} select
   * @param {HTMLOptionElement[]} options
   */
  _buildVisualPicker(select, options) {
    if (!this._pickerEl) return;
    const templateId = select.id.replace('block-type-', '');

    this._pickerEl.classList.add('picker--active');
    for (const opt of options) {
      this._pickerEl.appendChild(this._buildCard(opt, select, templateId));
    }
  }

  /**
   * Build a single visual-picker card for one option.
   *
   * @param {HTMLOptionElement} opt
   * @param {HTMLSelectElement} select
   * @param {string} templateId
   * @returns {HTMLButtonElement}
   */
  _buildCard(opt, select, templateId) {
    const label = opt.textContent || opt.value;
    const visual = opt.dataset.imageUrl
      ? h('img', { class: 'card__img', src: opt.dataset.imageUrl, alt: label })
      : h('span', { class: 'card__icon', text: 'widgets' });

    return h(
      'button',
      {
        type: 'button',
        class: 'card',
        onClick: () => {
          select.value = opt.value;
          this._requestAddBlock(templateId);
        },
      },
      visual,
      h('span', { class: 'card__label', text: label }),
    );
  }

  /** @param {string} templateId */
  _requestAddBlock(templateId) {
    this.dispatchEvent(
      new CustomEvent('crap:request-add-block', {
        bubbles: true,
        detail: { templateId },
      }),
    );
  }
}

customElements.define('crap-block-picker', CrapBlockPicker);
