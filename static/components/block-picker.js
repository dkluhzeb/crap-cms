/**
 * Block Picker Enhancements — optgroup grouping and visual card picker.
 *
 * Enhances `.form__blocks-select` elements on the page:
 * - If the parent `.form__blocks-add` has `data-picker="card"`, builds a visual card
 *   grid picker (shows images when `data-image-url` is set on options, icon fallback otherwise).
 * - Otherwise, if any `<option>` has a `data-group` attribute, reorganizes options into
 *   `<optgroup>` elements.
 *
 * Configure in Lua: `admin = { picker = "card" }` on a blocks field.
 *
 * @module block-picker
 */

/**
 * Enhance a single block-type select element.
 *
 * @param {HTMLSelectElement} select
 */
function enhanceBlockSelect(select) {
  const addContainer = select.closest('.form__blocks-add');
  if (!addContainer) return;

  // Skip if already enhanced
  if (addContainer.querySelector('.form__blocks-picker')) return;
  if (select.querySelector('optgroup')) return;

  const pickerMode = /** @type {HTMLElement} */ (addContainer).dataset.picker;
  const options = /** @type {HTMLOptionElement[]} */ ([...select.options]);
  const hasGroups = options.some((o) => o.dataset.group);

  if (pickerMode === 'card') {
    buildVisualPicker(select, options);
  } else if (hasGroups) {
    buildOptgroups(select, options);
  }
}

/**
 * Reorganize flat options into `<optgroup>` elements grouped by `data-group`.
 * Ungrouped options remain at the top.
 *
 * @param {HTMLSelectElement} select
 * @param {HTMLOptionElement[]} options
 */
function buildOptgroups(select, options) {
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

  // Only reorganize if there are actually grouped options
  if (groups.size === 0) return;

  // Clear and rebuild
  select.innerHTML = '';

  // Ungrouped first
  for (const opt of ungrouped) {
    select.appendChild(opt);
  }

  // Then each group
  for (const [name, opts] of groups) {
    const optgroup = document.createElement('optgroup');
    optgroup.label = name;
    for (const opt of opts) {
      optgroup.appendChild(opt);
    }
    select.appendChild(optgroup);
  }
}

/**
 * Build a visual card grid picker, hiding the original select + add button.
 * Each card shows the block's image (if available) or a fallback icon, plus its label.
 * Clicking a card adds a new row of that block type.
 *
 * @param {HTMLSelectElement} select
 * @param {HTMLOptionElement[]} options
 */
function buildVisualPicker(select, options) {
  const addContainer = select.closest('.form__blocks-add');
  if (!addContainer) return;

  // Extract templateId from the select's id: "block-type-{templateId}"
  const selectId = select.id;
  const templateId = selectId.replace('block-type-', '');

  // Find the add button for max_rows data
  const addBtn = addContainer.querySelector('[data-max-rows]');
  const maxRows = addBtn ? parseInt(/** @type {HTMLElement} */ (addBtn).dataset.maxRows, 10) : null;

  // Build the visual picker grid
  const picker = document.createElement('div');
  picker.className = 'form__blocks-picker';

  for (const opt of options) {
    const card = document.createElement('button');
    card.type = 'button';
    card.className = 'form__blocks-picker-card';
    card.dataset.blockType = opt.value;
    card.dataset.templateId = templateId;
    if (maxRows !== null) card.dataset.maxRows = String(maxRows);

    const imageUrl = opt.dataset.imageUrl;
    if (imageUrl) {
      const img = document.createElement('img');
      img.src = imageUrl;
      img.alt = opt.textContent || opt.value;
      img.className = 'form__blocks-picker-card-img';
      card.appendChild(img);
    } else {
      // Fallback: icon placeholder for blocks without images
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
      // Set select value and trigger addBlockRow
      select.value = opt.value;
      if (typeof window.addBlockRow === 'function') {
        window.addBlockRow(templateId);
      }
    });

    picker.appendChild(card);
  }

  // Hide the original select + add button, show visual picker instead
  addContainer.classList.add('form__blocks-add--has-picker');
  addContainer.insertBefore(picker, addContainer.firstChild);
}

/**
 * Initialize: enhance all block selects on the page.
 */
function init() {
  document.querySelectorAll('.form__blocks-select').forEach((el) => {
    enhanceBlockSelect(/** @type {HTMLSelectElement} */ (el));
  });
}

// Run on first load and after HTMX swaps (same pattern as other modules)
document.addEventListener('DOMContentLoaded', init);
document.addEventListener('htmx:afterSettle', init);
