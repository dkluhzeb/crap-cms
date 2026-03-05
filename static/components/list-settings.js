/**
 * List settings — column picker and filter builder for collection list views.
 *
 * Column picker: reads options from `#crap-column-options` JSON island,
 * renders checkbox list in drawer, POSTs selected columns to user settings API.
 *
 * Filter builder: reads field metadata from `#crap-filter-fields` JSON island,
 * builds filter rows in drawer, constructs `where[field][op]=value` URL params.
 *
 * @module list-settings
 */

import { registerAction, registerInit } from './actions.js';
import { getDrawer } from './drawer.js';
import { t } from './i18n.js';

/** @type {string|null} */
let currentSlug = null;

/**
 * Detect collection slug from current URL.
 * Matches `/admin/collections/{slug}` (not `/admin/collections/{slug}/{id}`).
 */
function detectSlug() {
  const match = window.location.pathname.match(/^\/admin\/collections\/([^/]+)\/?$/);
  currentSlug = match ? match[1] : null;
}

registerInit(detectSlug);

/* ── Search focus preservation ─────────────────────────────── */

/** @type {boolean} */
let searchWasActive = false;

document.addEventListener('htmx:beforeRequest', (e) => {
  const trigger = e.detail.elt;
  if (trigger && trigger.id === 'list-search-input') {
    searchWasActive = true;
  }
});

document.addEventListener('htmx:afterSettle', () => {
  if (!searchWasActive) return;
  searchWasActive = false;
  const input = /** @type {HTMLInputElement|null} */ (
    document.getElementById('list-search-input')
  );
  if (input) {
    input.focus();
    input.setSelectionRange(input.value.length, input.value.length);
  }
});

/* ── Column Picker ──────────────────────────────────────────── */

registerAction('open-column-picker', () => {
  const island = document.getElementById('crap-column-options');
  if (!island || !currentSlug) return;

  /** @type {Array<{key: string, label: string, selected: boolean}>} */
  const options = JSON.parse(island.textContent || '[]');

  const drawer = getDrawer();
  drawer.open({ title: t('columns') });

  const body = drawer.body;
  body.innerHTML = '';

  const form = document.createElement('form');
  form.className = 'column-picker';

  const list = document.createElement('div');
  list.className = 'column-picker__list';

  for (const opt of options) {
    const label = document.createElement('label');
    label.className = 'column-picker__item';

    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    checkbox.name = 'column';
    checkbox.value = opt.key;
    checkbox.checked = opt.selected;

    const text = document.createElement('span');
    text.textContent = opt.label;

    label.appendChild(checkbox);
    label.appendChild(text);
    list.appendChild(label);
  }
  form.appendChild(list);

  const footer = document.createElement('div');
  footer.className = 'column-picker__footer';

  const saveBtn = document.createElement('button');
  saveBtn.type = 'submit';
  saveBtn.className = 'button button--primary button--small';
  saveBtn.textContent = t('save');
  footer.appendChild(saveBtn);

  form.appendChild(footer);
  body.appendChild(form);

  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const checked = /** @type {NodeListOf<HTMLInputElement>} */ (
      form.querySelectorAll('input[name="column"]:checked')
    );
    const columns = Array.from(checked).map(cb => cb.value).join(',');

    const csrfCookie = document.cookie.split(';')
      .map(c => c.trim())
      .find(c => c.startsWith('crap_csrf='));
    const csrf = csrfCookie ? csrfCookie.split('=')[1] : '';

    try {
      const resp = await fetch(`/admin/api/user-settings/${currentSlug}`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/x-www-form-urlencoded',
          'X-CSRF-Token': csrf,
        },
        body: `columns=${encodeURIComponent(columns)}`,
      });
      if (resp.ok) {
        drawer.close();
        window.location.reload();
      }
    } catch {
      // Silently fail — user can retry
    }
  });
});

/* ── Filter Builder ─────────────────────────────────────────── */

/** Operator options per field type. */
const OPS_BY_TYPE = {
  text:     [['equals', 'is'], ['not_equals', 'is not'], ['contains', 'contains']],
  email:    [['equals', 'is'], ['not_equals', 'is not'], ['contains', 'contains']],
  textarea: [['equals', 'is'], ['contains', 'contains']],
  number:   [['equals', '='], ['gt', '>'], ['lt', '<'], ['gte', '>='], ['lte', '<=']],
  select:   [['equals', 'is'], ['not_equals', 'is not']],
  radio:    [['equals', 'is'], ['not_equals', 'is not']],
  checkbox: [['equals', 'is']],
  date:     [['equals', 'is'], ['gt', 'after'], ['lt', 'before'], ['gte', 'on or after'], ['lte', 'on or before']],
  relationship: [['equals', 'is'], ['not_equals', 'is not'], ['exists', 'exists'], ['not_exists', 'not exists']],
  upload:   [['exists', 'exists'], ['not_exists', 'not exists']],
};

/**
 * Parse current URL for existing where params.
 * @returns {Array<{field: string, op: string, value: string}>}
 */
function parseCurrentFilters() {
  const params = new URLSearchParams(window.location.search);
  /** @type {Array<{field: string, op: string, value: string}>} */
  const filters = [];
  for (const [key, value] of params) {
    const match = key.match(/^where\[([^\]]+)\]\[([^\]]+)\]$/);
    if (match) {
      filters.push({ field: match[1], op: match[2], value });
    }
  }
  return filters;
}

/**
 * Build a value input element appropriate for the field type.
 * @param {{key: string, label: string, field_type: string, options?: Array<{label: string, value: string}>}} fieldMeta
 * @param {string} op
 * @param {string} [currentValue]
 * @returns {HTMLElement}
 */
function buildValueInput(fieldMeta, op, currentValue = '') {
  if (op === 'exists' || op === 'not_exists') {
    const span = document.createElement('span');
    return span;
  }

  if (fieldMeta.options && (fieldMeta.field_type === 'select' || fieldMeta.field_type === 'radio')) {
    const select = document.createElement('select');
    select.name = 'filter-value';
    for (const opt of fieldMeta.options) {
      const option = document.createElement('option');
      option.value = opt.value;
      option.textContent = opt.label;
      if (opt.value === currentValue) option.selected = true;
      select.appendChild(option);
    }
    return select;
  }

  if (fieldMeta.field_type === 'checkbox') {
    const select = document.createElement('select');
    select.name = 'filter-value';
    for (const [val, label] of [['1', t('yes')], ['0', t('no')]]) {
      const option = document.createElement('option');
      option.value = val;
      option.textContent = label;
      if (val === currentValue) option.selected = true;
      select.appendChild(option);
    }
    return select;
  }

  const input = document.createElement('input');
  input.name = 'filter-value';
  input.value = currentValue;

  if (fieldMeta.field_type === 'number') {
    input.type = 'number';
    input.step = 'any';
  } else if (fieldMeta.field_type === 'date') {
    input.type = 'date';
  } else {
    input.type = 'text';
    input.placeholder = t('value_placeholder');
  }

  return input;
}

registerAction('open-filter-builder', () => {
  const island = document.getElementById('crap-filter-fields');
  if (!island || !currentSlug) return;

  /** @type {Array<{key: string, label: string, field_type: string, options?: Array<{label: string, value: string}>}>} */
  const fieldMetas = JSON.parse(island.textContent || '[]');
  if (!fieldMetas.length) return;

  const existing = parseCurrentFilters();

  const drawer = getDrawer();
  drawer.open({ title: t('filters') });

  const body = drawer.body;
  body.innerHTML = '';

  const container = document.createElement('div');
  container.className = 'filter-builder';

  const rows = document.createElement('div');
  rows.className = 'filter-builder__rows';

  /** @param {{field: string, op: string, value: string}|null} preset */
  function addRow(preset = null) {
    const row = document.createElement('div');
    row.className = 'filter-builder__row';

    // Field select
    const fieldSelect = document.createElement('select');
    fieldSelect.className = 'filter-builder__field';
    fieldSelect.name = 'filter-field';
    for (const fm of fieldMetas) {
      const opt = document.createElement('option');
      opt.value = fm.key;
      opt.textContent = fm.label;
      if (preset && fm.key === preset.field) opt.selected = true;
      fieldSelect.appendChild(opt);
    }

    // Op select
    const opSelect = document.createElement('select');
    opSelect.className = 'filter-builder__op';
    opSelect.name = 'filter-op';

    /** @param {string} fieldKey */
    function updateOps(fieldKey) {
      const fm = fieldMetas.find(f => f.key === fieldKey);
      const ft = fm ? fm.field_type : 'text';
      const ops = OPS_BY_TYPE[ft] || OPS_BY_TYPE.text;
      opSelect.innerHTML = '';
      for (const [val, label] of ops) {
        const opt = document.createElement('option');
        opt.value = val;
        opt.textContent = label;
        if (preset && val === preset.op) opt.selected = true;
        opSelect.appendChild(opt);
      }
    }

    updateOps(fieldSelect.value);
    fieldSelect.addEventListener('change', () => {
      updateOps(fieldSelect.value);
      updateValue();
    });

    // Value input
    const valueWrap = document.createElement('div');
    valueWrap.className = 'filter-builder__value-wrap';

    function updateValue() {
      const fm = fieldMetas.find(f => f.key === fieldSelect.value);
      if (!fm) return;
      valueWrap.innerHTML = '';
      valueWrap.appendChild(buildValueInput(fm, opSelect.value, preset ? preset.value : ''));
    }

    updateValue();
    opSelect.addEventListener('change', updateValue);

    // Remove button
    const removeBtn = document.createElement('button');
    removeBtn.type = 'button';
    removeBtn.className = 'button button--ghost button--small filter-builder__remove';
    removeBtn.innerHTML = '<span class="material-symbols-outlined">close</span>';
    removeBtn.addEventListener('click', () => row.remove());

    row.appendChild(fieldSelect);
    row.appendChild(opSelect);
    row.appendChild(valueWrap);
    row.appendChild(removeBtn);
    rows.appendChild(row);
  }

  // Pre-populate from existing filters
  if (existing.length > 0) {
    for (const f of existing) addRow(f);
  } else {
    addRow();
  }

  container.appendChild(rows);

  // Add filter button
  const addBtn = document.createElement('button');
  addBtn.type = 'button';
  addBtn.className = 'button button--ghost button--small';
  addBtn.innerHTML = '<span class="material-symbols-outlined">add</span> ' + t('add_condition');
  addBtn.addEventListener('click', () => addRow());
  container.appendChild(addBtn);

  // Footer
  const footer = document.createElement('div');
  footer.className = 'filter-builder__footer';

  const clearBtn = document.createElement('a');
  clearBtn.className = 'button button--ghost button--small';
  clearBtn.textContent = t('clear_all');
  clearBtn.href = `/admin/collections/${currentSlug}`;
  footer.appendChild(clearBtn);

  const applyBtn = document.createElement('button');
  applyBtn.type = 'button';
  applyBtn.className = 'button button--primary button--small';
  applyBtn.textContent = t('apply');
  applyBtn.addEventListener('click', () => {
    const url = new URL(window.location.href);
    // Remove existing where params
    const keysToRemove = [];
    for (const key of url.searchParams.keys()) {
      if (key.startsWith('where[')) keysToRemove.push(key);
    }
    for (const key of keysToRemove) url.searchParams.delete(key);

    // Reset to page 1
    url.searchParams.set('page', '1');

    // Add new filters
    const filterRows = rows.querySelectorAll('.filter-builder__row');
    for (const row of filterRows) {
      const field = /** @type {HTMLSelectElement} */ (row.querySelector('.filter-builder__field')).value;
      const op = /** @type {HTMLSelectElement} */ (row.querySelector('.filter-builder__op')).value;
      const valueEl = row.querySelector('[name="filter-value"]');
      const value = valueEl ? /** @type {HTMLInputElement} */ (valueEl).value : '';
      url.searchParams.append(`where[${field}][${op}]`, value);
    }

    drawer.close();
    // Navigate via HTMX if available, otherwise location
    if (window.htmx) {
      htmx.ajax('GET', url.pathname + url.search, { target: 'body', pushUrl: true });
    } else {
      window.location.href = url.toString();
    }
  });
  footer.appendChild(applyBtn);

  container.appendChild(footer);
  body.appendChild(container);
});
