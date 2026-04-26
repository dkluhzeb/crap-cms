/**
 * List settings — `<crap-list-settings>`.
 *
 * Toolbar component for collection list views. Provides:
 *   - **Column picker** drawer — pick which columns the list table shows.
 *   - **Filter builder** drawer — compose `where[field][op]=value` URL
 *     filters using per-type operator menus and value inputs.
 *
 * Both drawers borrow `<crap-drawer>` via the `crap:drawer-request`
 * discovery event. Field metadata + saved selections come from JSON
 * data islands the server renders into the page.
 *
 * Light DOM — operates on the server-rendered list page and uses
 * HTMX-aware navigation for filter application.
 *
 * @module list-settings
 */

import { clear, h } from './h.js';
import { t } from './i18n.js';
import { readCsrfCookie } from './util/cookies.js';
import { discoverSingleton } from './util/discover.js';
import { readDataIsland } from './util/json.js';

/**
 * Operator options per field type. Tuple shape: `[opValue, labelKey]`.
 * Labels are translation keys resolved at render time via `t()`.
 *
 * @type {Record<string, ReadonlyArray<readonly [string, string]>>}
 */
const OPS_BY_TYPE = {
  text: [
    ['equals', 'op_is'],
    ['not_equals', 'op_is_not'],
    ['contains', 'op_contains'],
  ],
  email: [
    ['equals', 'op_is'],
    ['not_equals', 'op_is_not'],
    ['contains', 'op_contains'],
  ],
  textarea: [
    ['equals', 'op_is'],
    ['contains', 'op_contains'],
  ],
  number: [
    ['equals', 'op_equals'],
    ['gt', 'op_gt'],
    ['lt', 'op_lt'],
    ['gte', 'op_gte'],
    ['lte', 'op_lte'],
  ],
  select: [
    ['equals', 'op_is'],
    ['not_equals', 'op_is_not'],
  ],
  radio: [
    ['equals', 'op_is'],
    ['not_equals', 'op_is_not'],
  ],
  checkbox: [['equals', 'op_is']],
  date: [
    ['equals', 'op_is'],
    ['gt', 'op_after'],
    ['lt', 'op_before'],
    ['gte', 'op_on_or_after'],
    ['lte', 'op_on_or_before'],
  ],
  relationship: [
    ['equals', 'op_is'],
    ['not_equals', 'op_is_not'],
    ['exists', 'op_exists'],
    ['not_exists', 'op_not_exists'],
  ],
  upload: [
    ['exists', 'op_exists'],
    ['not_exists', 'op_not_exists'],
  ],
};

/** Operators that take no value input. */
const NO_VALUE_OPS = new Set(['exists', 'not_exists']);

/**
 * @typedef {{ key: string, label: string, selected: boolean }} ColumnOption
 *
 * @typedef {{ label: string, value: string }} SelectOption
 *
 * @typedef {{
 *   key: string,
 *   label: string,
 *   field_type: string,
 *   options?: SelectOption[],
 * }} FieldMeta
 *
 * @typedef {{ field: string, op: string, value: string }} Filter
 *
 * @typedef {{
 *   open: (opts: { title: string }) => void,
 *   close: () => void,
 *   body: HTMLElement,
 * }} DrawerInstance
 */

/**
 * HTMX-aware navigation. Falls back to a full reload if HTMX isn't loaded.
 *
 * @param {URL} url
 */
function navigate(url) {
  if (typeof htmx !== 'undefined') {
    htmx.ajax('GET', url.pathname + url.search, { target: 'body', pushUrl: true });
    return;
  }
  window.location.href = url.toString();
}

class CrapListSettings extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {boolean} */
    this._searchWasActive = false;
    /** @type {((e: Event) => void)|null} */
    this._onBeforeRequest = null;
    /** @type {(() => void)|null} */
    this._onAfterSettle = null;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    this.addEventListener('click', (e) => this._onToolbarClick(e));
    this._setupSearchFocusPreservation();
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    if (this._onBeforeRequest)
      document.removeEventListener('htmx:beforeRequest', this._onBeforeRequest);
    if (this._onAfterSettle) document.removeEventListener('htmx:afterSettle', this._onAfterSettle);
  }

  /** Collection slug from the current URL, or `null` if not on a list page. */
  get _slug() {
    const m = window.location.pathname.match(/^\/admin\/collections\/([^/]+)\/?$/);
    return m ? m[1] : null;
  }

  /** @param {Event} e */
  _onToolbarClick(e) {
    if (!(e.target instanceof Element)) return;
    const btn = /** @type {HTMLElement|null} */ (e.target.closest('[data-action]'));
    if (!btn) return;
    switch (btn.dataset.action) {
      case 'open-column-picker':
        this._openColumnPicker();
        break;
      case 'open-filter-builder':
        this._openFilterBuilder();
        break;
    }
  }

  /**
   * Restore focus + caret position to the list-view search input across
   * HTMX list swaps. Without this, the input loses focus on every
   * keystroke that triggers a server-side search refresh.
   */
  _setupSearchFocusPreservation() {
    this._onBeforeRequest = (e) => {
      const detail = /** @type {CustomEvent} */ (e).detail;
      if (detail.elt?.id === 'list-search-input') this._searchWasActive = true;
    };
    this._onAfterSettle = () => {
      if (!this._searchWasActive) return;
      this._searchWasActive = false;
      const input = /** @type {HTMLInputElement|null} */ (
        document.getElementById('list-search-input')
      );
      if (!input) return;
      input.focus();
      input.setSelectionRange(input.value.length, input.value.length);
    };
    document.addEventListener('htmx:beforeRequest', this._onBeforeRequest);
    document.addEventListener('htmx:afterSettle', this._onAfterSettle);
  }

  /* ── Column picker ──────────────────────────────────────────── */

  _openColumnPicker() {
    const slug = this._slug;
    if (!slug) return;
    /** @type {ColumnOption[]} */
    const options = readDataIsland(this, 'crap-column-options', []);
    const drawer = discoverSingleton('crap:drawer-request');
    if (!drawer) return;
    drawer.open({ title: t('columns') });
    clear(drawer.body);
    drawer.body.appendChild(this._buildColumnPickerForm(options, slug, drawer));
  }

  /**
   * @param {ColumnOption[]} options
   * @param {string} slug
   * @param {DrawerInstance} drawer
   */
  _buildColumnPickerForm(options, slug, drawer) {
    const form = h(
      'form',
      { class: 'column-picker' },
      h(
        'div',
        { class: 'column-picker__list' },
        options.map((opt) =>
          h(
            'label',
            { class: 'column-picker__item' },
            h('input', { type: 'checkbox', name: 'column', value: opt.key, checked: opt.selected }),
            h('span', { text: t(opt.label) }),
          ),
        ),
      ),
      h(
        'div',
        { class: 'column-picker__footer' },
        h('button', {
          type: 'submit',
          class: ['button', 'button--primary', 'button--small'],
          text: t('save'),
        }),
      ),
    );
    form.addEventListener('submit', (e) => {
      e.preventDefault();
      this._saveColumnSelection(form, slug, drawer);
    });
    return form;
  }

  /**
   * @param {HTMLFormElement} form
   * @param {string} slug
   * @param {DrawerInstance} drawer
   */
  async _saveColumnSelection(form, slug, drawer) {
    const checked = /** @type {NodeListOf<HTMLInputElement>} */ (
      form.querySelectorAll('input[name="column"]:checked')
    );
    const columns = [...checked].map((cb) => cb.value).join(',');
    try {
      const resp = await fetch(`/admin/api/user-settings/${slug}`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/x-www-form-urlencoded',
          'X-CSRF-Token': readCsrfCookie(),
        },
        body: `columns=${encodeURIComponent(columns)}`,
      });
      if (!resp.ok) return;
      drawer.close();
      window.location.reload();
    } catch {
      // Silent — user can retry.
    }
  }

  /* ── Filter builder ─────────────────────────────────────────── */

  _openFilterBuilder() {
    const slug = this._slug;
    if (!slug) return;
    /** @type {FieldMeta[]} */
    const fieldMetas = readDataIsland(this, 'crap-filter-fields', []);
    if (!fieldMetas.length) return;
    const drawer = discoverSingleton('crap:drawer-request');
    if (!drawer) return;
    drawer.open({ title: t('filters') });
    clear(drawer.body);
    drawer.body.appendChild(this._buildFilterUI(fieldMetas, slug, drawer));
  }

  /**
   * @param {FieldMeta[]} fieldMetas
   * @param {string} slug
   * @param {DrawerInstance} drawer
   */
  _buildFilterUI(fieldMetas, slug, drawer) {
    const presets = this._parseCurrentFilters();
    const initial = presets.length > 0 ? presets : [null];
    const rowsEl = h(
      'div',
      { class: 'filter-builder__rows' },
      initial.map((p) => this._buildFilterRow(fieldMetas, p)),
    );

    return h(
      'div',
      { class: 'filter-builder' },
      rowsEl,
      this._buildAddRowButton(rowsEl, fieldMetas),
      this._buildFilterFooter(rowsEl, slug, drawer),
    );
  }

  /**
   * @param {HTMLElement} rowsEl
   * @param {FieldMeta[]} fieldMetas
   */
  _buildAddRowButton(rowsEl, fieldMetas) {
    return h(
      'button',
      {
        type: 'button',
        class: ['button', 'button--ghost', 'button--small'],
        onClick: () => rowsEl.appendChild(this._buildFilterRow(fieldMetas, null)),
      },
      h('span', { class: 'material-symbols-outlined', text: 'add' }),
      ` ${t('add_condition')}`,
    );
  }

  /**
   * @param {HTMLElement} rowsEl
   * @param {string} slug
   * @param {DrawerInstance} drawer
   */
  _buildFilterFooter(rowsEl, slug, drawer) {
    return h(
      'div',
      { class: 'filter-builder__footer' },
      h('a', {
        class: ['button', 'button--ghost', 'button--small'],
        href: `/admin/collections/${slug}`,
        text: t('clear_all'),
      }),
      h('button', {
        type: 'button',
        class: ['button', 'button--primary', 'button--small'],
        text: t('apply'),
        onClick: () => this._applyFilters(rowsEl, drawer),
      }),
    );
  }

  /**
   * Build one filter row: field select + op select + value input + remove.
   * The op + value inputs re-render reactively on field/op change.
   *
   * @param {FieldMeta[]} fieldMetas
   * @param {Filter|null} preset
   */
  _buildFilterRow(fieldMetas, preset) {
    const fieldSelect = this._buildFieldSelect(fieldMetas, preset);
    const opSelect = h('select', { class: 'filter-builder__op', name: 'filter-op' });
    const valueWrap = h('div', { class: 'filter-builder__value-wrap' });

    const renderOp = () => {
      const fm = fieldMetas.find((f) => f.key === fieldSelect.value);
      this._renderOpsInto(opSelect, fm?.field_type || 'text', preset?.op);
    };
    const renderValue = () => {
      const fm = fieldMetas.find((f) => f.key === fieldSelect.value);
      if (fm) this._renderValueInto(valueWrap, fm, opSelect.value, preset?.value || '');
    };

    renderOp();
    renderValue();
    fieldSelect.addEventListener('change', () => {
      renderOp();
      renderValue();
    });
    opSelect.addEventListener('change', renderValue);

    const row = h('div', { class: 'filter-builder__row' });
    const removeBtn = h(
      'button',
      {
        type: 'button',
        class: ['button', 'button--ghost', 'button--small', 'filter-builder__remove'],
        onClick: () => row.remove(),
      },
      h('span', { class: 'material-symbols-outlined', text: 'close' }),
    );
    row.append(fieldSelect, opSelect, valueWrap, removeBtn);
    return row;
  }

  /**
   * @param {FieldMeta[]} fieldMetas
   * @param {Filter|null} preset
   */
  _buildFieldSelect(fieldMetas, preset) {
    return h(
      'select',
      {
        class: 'filter-builder__field',
        name: 'filter-field',
      },
      fieldMetas.map((fm) =>
        h('option', {
          value: fm.key,
          selected: !!(preset && fm.key === preset.field),
          text: t(fm.label),
        }),
      ),
    );
  }

  /**
   * @param {HTMLSelectElement} opSelect
   * @param {string} fieldType
   * @param {string|undefined} currentOp
   */
  _renderOpsInto(opSelect, fieldType, currentOp) {
    const ops = OPS_BY_TYPE[fieldType] || OPS_BY_TYPE.text;
    opSelect.replaceChildren(
      ...ops.map(([val, label]) =>
        h('option', {
          value: val,
          selected: val === currentOp,
          text: t(label),
        }),
      ),
    );
  }

  /**
   * @param {HTMLElement} valueWrap
   * @param {FieldMeta} fm
   * @param {string} op
   * @param {string} currentValue
   */
  _renderValueInto(valueWrap, fm, op, currentValue) {
    valueWrap.replaceChildren(this._buildValueInput(fm, op, currentValue));
  }

  /**
   * Build the value input/select for a single filter row. Returns an
   * empty `<span>` for `exists`/`not_exists` operators.
   *
   * @param {FieldMeta} fm
   * @param {string} op
   * @param {string} currentValue
   * @returns {HTMLElement}
   */
  _buildValueInput(fm, op, currentValue) {
    if (NO_VALUE_OPS.has(op)) return h('span');
    if (fm.options && (fm.field_type === 'select' || fm.field_type === 'radio')) {
      return this._buildSelectInput(fm.options, currentValue);
    }
    if (fm.field_type === 'checkbox') {
      return this._buildBooleanSelect(currentValue);
    }
    return this._buildTextInput(fm.field_type, currentValue);
  }

  /**
   * @param {SelectOption[]} options
   * @param {string} currentValue
   */
  _buildSelectInput(options, currentValue) {
    return h(
      'select',
      { name: 'filter-value' },
      options.map((opt) =>
        h('option', {
          value: opt.value,
          selected: opt.value === currentValue,
          text: t(opt.label),
        }),
      ),
    );
  }

  /** @param {string} currentValue */
  _buildBooleanSelect(currentValue) {
    return h(
      'select',
      { name: 'filter-value' },
      h('option', { value: '1', selected: currentValue === '1', text: t('yes') }),
      h('option', { value: '0', selected: currentValue === '0', text: t('no') }),
    );
  }

  /**
   * @param {string} fieldType
   * @param {string} currentValue
   */
  _buildTextInput(fieldType, currentValue) {
    if (fieldType === 'number') {
      return h('input', { name: 'filter-value', type: 'number', step: 'any', value: currentValue });
    }
    if (fieldType === 'date') {
      return h('input', { name: 'filter-value', type: 'date', value: currentValue });
    }
    return h('input', {
      name: 'filter-value',
      type: 'text',
      value: currentValue,
      placeholder: t('value_placeholder'),
    });
  }

  /**
   * Read the current `where[field][op]=value` filters off the URL.
   * @returns {Filter[]}
   */
  _parseCurrentFilters() {
    /** @type {Filter[]} */
    const filters = [];
    for (const [key, value] of new URLSearchParams(window.location.search)) {
      const m = key.match(/^where\[([^\]]+)\]\[([^\]]+)\]$/);
      if (m) filters.push({ field: m[1], op: m[2], value });
    }
    return filters;
  }

  /**
   * Apply rows: rebuild the URL's `where[…]` params, reset to page 1,
   * navigate via HTMX (or full reload if HTMX is absent).
   *
   * @param {HTMLElement} rowsEl
   * @param {DrawerInstance} drawer
   */
  _applyFilters(rowsEl, drawer) {
    drawer.close();
    navigate(this._buildFilterUrl(rowsEl));
  }

  /**
   * Build the filtered list URL: drop existing `where[…]` params, reset
   * to page 1, then append one entry per filter row.
   *
   * @param {HTMLElement} rowsEl
   */
  _buildFilterUrl(rowsEl) {
    const url = new URL(window.location.href);
    for (const key of [...url.searchParams.keys()]) {
      if (key.startsWith('where[')) url.searchParams.delete(key);
    }
    url.searchParams.set('page', '1');
    for (const f of this._collectFilters(rowsEl)) {
      url.searchParams.append(`where[${f.field}][${f.op}]`, f.value);
    }
    return url;
  }

  /**
   * Read the current value of every filter row in `rowsEl`.
   *
   * @param {HTMLElement} rowsEl
   * @returns {Filter[]}
   */
  _collectFilters(rowsEl) {
    /** @type {Filter[]} */
    const filters = [];
    for (const row of rowsEl.querySelectorAll('.filter-builder__row')) {
      const fieldEl = /** @type {HTMLSelectElement|null} */ (
        row.querySelector('.filter-builder__field')
      );
      const opEl = /** @type {HTMLSelectElement|null} */ (row.querySelector('.filter-builder__op'));
      if (!fieldEl || !opEl) continue;
      const valueEl = /** @type {HTMLInputElement|null} */ (
        row.querySelector('[name="filter-value"]')
      );
      filters.push({ field: fieldEl.value, op: opEl.value, value: valueEl?.value || '' });
    }
    return filters;
  }
}

customElements.define('crap-list-settings', CrapListSettings);
