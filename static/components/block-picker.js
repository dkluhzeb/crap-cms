/**
 * Block Picker — `<crap-block-picker>`.
 *
 * Enhances block-type select elements with optgroup grouping or visual
 * card picker. Dispatches `crap:request-add-block` CustomEvent to
 * request the parent `<crap-array-field>` add a row.
 *
 * @module block-picker
 */

class CrapBlockPicker extends HTMLElement {
  connectedCallback() {
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
    if (this.querySelector('.form__blocks-picker')) return;
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

    select.innerHTML = '';
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

    const picker = document.createElement('div');
    picker.className = 'form__blocks-picker';

    for (const opt of options) {
      const card = document.createElement('button');
      card.type = 'button';
      card.className = 'form__blocks-picker-card';

      const imageUrl = opt.dataset.imageUrl;
      if (imageUrl) {
        const img = document.createElement('img');
        img.src = imageUrl;
        img.alt = opt.textContent || opt.value;
        img.className = 'form__blocks-picker-card-img';
        card.appendChild(img);
      } else {
        const icon = document.createElement('span');
        icon.className = 'material-symbols-outlined form__blocks-picker-card-icon';
        icon.textContent = 'widgets';
        card.appendChild(icon);
      }

      const label = document.createElement('span');
      label.className = 'form__blocks-picker-card-label';
      label.textContent = opt.textContent || opt.value;
      card.appendChild(label);

      card.addEventListener('click', () => {
        select.value = opt.value;
        this.dispatchEvent(new CustomEvent('crap:request-add-block', {
          bubbles: true,
          detail: { templateId },
        }));
      });

      picker.appendChild(card);
    }

    this.classList.add('form__blocks-add--has-picker');
    this.insertBefore(picker, this.firstChild);
  }
}

customElements.define('crap-block-picker', CrapBlockPicker);
