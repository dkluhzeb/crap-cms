/**
 * Filter builder — `<crap-filter-builder>`.
 *
 * Renders a row-per-condition filter UI into itself. Each row is
 * connector + field + op + value + remove. Apply builds a `where[…]`
 * URL and navigates via htmx.
 *
 * Mounted dynamically by `<crap-list-settings>` inside the page-singleton
 * `<crap-drawer>` body. The orchestrator constructs the element with
 * the data already attached:
 *
 * @example
 *   const builder = document.createElement('crap-filter-builder');
 *   builder.dataset.collection = 'posts';
 *   builder.dataset.fields = JSON.stringify(fieldMetas);  // FieldMeta[]
 *   drawer.body.appendChild(builder);
 *
 * @attr data-collection  Collection slug — `clear-all` link target.
 * @attr data-fields      JSON-encoded `FieldMeta[]` list.
 *
 * Override pattern: drop a replacement at
 * `<config_dir>/static/components/list-settings/filter-builder.js` for
 * a full replace, or subclass `CrapFilterBuilder` (re-exported) for
 * incremental customization (custom value inputs, extra operators,
 * etc.).
 *
 * @module list-settings/filter-builder
 * @stability stable
 */

import { clear, h } from '../_internal/h.js';
import { t } from '../_internal/i18n.js';

/**
 * Operator options per field type. Tuple shape: `[opValue, labelKey]`.
 * Labels are translation keys resolved at render time via `t()`.
 *
 * Exported so subclasses / overlays can extend the table for custom
 * field types or add operators (e.g. a `between` for number ranges).
 *
 * @type {Record<string, ReadonlyArray<readonly [string, string]>>}
 */
export const OPS_BY_TYPE = {
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
 * @typedef {{ label: string, value: string }} SelectOption
 *
 * @typedef {{
 *   key: string,
 *   label: string,
 *   field_type: string,
 *   options?: SelectOption[],
 * }} FieldMeta
 *
 * @typedef {{ field: string, op: string, value: string, connector?: 'AND'|'OR' }} Filter
 */

/**
 * HTMX-aware navigation. Falls back to a full reload if HTMX isn't loaded.
 *
 * @param {URL} url
 */
function navigate(url) {
  if (typeof htmx !== 'undefined') {
    // htmx 2 renamed the URL-push option from `pushUrl: true` (1.x) to
    // `push: <string>` — the value is either the literal `"true"` or
    // the path to push. `pushUrl` is silently dropped; `push: true`
    // (boolean) gets coerced to the string `"true"` and pushed as the
    // URL `"/.../true"`. We pass the actual path so history matches
    // what was loaded.
    const path = url.pathname + url.search;
    htmx.ajax('GET', path, { target: 'body', push: path });
    return;
  }
  window.location.href = url.toString();
}

export class CrapFilterBuilder extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    const slug = this.dataset.collection;
    if (!slug) return;
    const fieldMetas = this._readFields();
    if (!fieldMetas.length) return;

    clear(this);
    this.appendChild(this._buildUI(fieldMetas, slug));
  }

  /**
   * Read the JSON-encoded field-metadata list from `data-fields`.
   * Returns `[]` if the attribute is missing or unparseable.
   *
   * @returns {FieldMeta[]}
   */
  _readFields() {
    const raw = this.getAttribute('data-fields');
    if (!raw) return [];
    try {
      const parsed = JSON.parse(raw);
      return Array.isArray(parsed) ? parsed : [];
    } catch {
      return [];
    }
  }

  /**
   * @param {FieldMeta[]} fieldMetas
   * @param {string} slug
   */
  _buildUI(fieldMetas, slug) {
    const presets = this._parseCurrentFilters();
    // No URL filters → builder opens with zero rows. The "+ Add condition"
    // button below is the affordance for adding the first filter. A
    // null-preset row would auto-hydrate to the first field's first op
    // and first value (typically `_status = "published"` for collections
    // with drafts), so clicking Apply without configuring would silently
    // apply that filter — which is *not* what the user intended.
    const rowsEl = h(
      'div',
      { class: 'filter-builder__rows' },
      presets.map((p) => this._buildRow(fieldMetas, p)),
    );

    return h(
      'div',
      { class: 'filter-builder' },
      rowsEl,
      this._buildAddRowButton(rowsEl, fieldMetas),
      this._buildFooter(rowsEl, slug),
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
        onClick: () => rowsEl.appendChild(this._buildRow(fieldMetas, null)),
      },
      h('span', { class: 'material-symbols-outlined', text: 'add' }),
      ` ${t('add_condition')}`,
    );
  }

  /**
   * @param {HTMLElement} rowsEl
   * @param {string} slug
   */
  _buildFooter(rowsEl, slug) {
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
        onClick: () => this._applyFilters(rowsEl),
      }),
    );
  }

  /**
   * Build one filter row: connector + field select + op select + value input
   * + remove. The connector dropdown sits at the front and is hidden on the
   * very first row (no previous row to connect to). The op + value inputs
   * re-render reactively on field/op change.
   *
   * @param {FieldMeta[]} fieldMetas
   * @param {Filter|null} preset
   */
  _buildRow(fieldMetas, preset) {
    const connectorSelect = this._buildConnectorSelect(preset);
    const fieldSelect = this._buildFieldSelect(fieldMetas, preset);
    const opSelect = h('select', { class: 'filter-builder__op', name: 'filter-op' });
    const valueWrap = h('div', { class: 'filter-builder__value-wrap' });

    // `preset` reflects the URL-derived state at row-construction time;
    // it doesn't update with user input. When the user changes the
    // field or op, we re-render the op list and value input — and we
    // must preserve whatever the user has just typed/selected, not
    // restore the stale preset. Read the current DOM state first;
    // fall back to the preset only when the input doesn't yet exist
    // (initial render) or has no value.
    const renderOp = () => {
      const fm = fieldMetas.find((f) => f.key === fieldSelect.value);
      const currentOp = opSelect.value || preset?.op;
      this._renderOpsInto(opSelect, fm?.field_type || 'text', currentOp);
    };
    const renderValue = () => {
      const fm = fieldMetas.find((f) => f.key === fieldSelect.value);
      if (!fm) return;
      const currentInput = /** @type {HTMLInputElement|HTMLSelectElement|null} */ (
        valueWrap.querySelector('[name="filter-value"]')
      );
      const currentValue = currentInput?.value || preset?.value || '';
      this._renderValueInto(valueWrap, fm, opSelect.value, currentValue);
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
    row.append(connectorSelect, fieldSelect, opSelect, valueWrap, removeBtn);
    return row;
  }

  /**
   * Connector dropdown rendered on every row. The first row's connector is
   * visually hidden (CSS `aria-hidden`) but remains in the DOM so subsequent
   * row insertions don't need to reflow positions. Default `AND`.
   *
   * @param {Filter|null} preset
   */
  _buildConnectorSelect(preset) {
    const value = preset?.connector === 'OR' ? 'OR' : 'AND';
    return h(
      'select',
      { class: 'filter-builder__connector', name: 'filter-connector' },
      h('option', { value: 'AND', selected: value === 'AND', text: t('op_and') }),
      h('option', { value: 'OR', selected: value === 'OR', text: t('op_or') }),
    );
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
   * Override hook: subclasses can change which operators are available
   * for a given field type by overriding this method (e.g. add a
   * `between` to the number list).
   *
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
   * Override hook: subclasses can swap the value-input shape (e.g.
   * a fancy date-range picker for `date` fields, an autocomplete
   * for `relationship` fields).
   *
   * Default: select for fields with options, boolean select for
   * checkbox, type-appropriate input for everything else.
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
   * Read the current `where[…]` URL params and render them as a flat list of
   * rows ready to feed `_buildRow`. Recognises both the top-level form
   * (`where[field][op]=value` → row with connector `AND`) and the OR form
   * (`where[or][G][N][field][op]=value`). For the OR form the *first* bucket
   * (`N=0`) of each OR-clause renders as connector `AND` — it's the row that
   * "broke" from the preceding AND-flow into a new OR-streak — and subsequent
   * buckets (`N>0`) render as connector `OR` to extend the streak. Rows from
   * different OR-clauses (different `G`) are AND'd between OR-streaks.
   *
   * @returns {Filter[]}
   */
  _parseCurrentFilters() {
    /** @type {Filter[]} */
    const filters = [];
    for (const [key, value] of new URLSearchParams(window.location.search)) {
      // OR form: `where[or][G][N][field][op]`.
      const orMatch = key.match(/^where\[or\]\[(\d+)\]\[(\d+)\]\[([^\]]+)\]\[([^\]]+)\]$/);
      if (orMatch) {
        const bucket = Number(orMatch[2]);
        filters.push({
          field: orMatch[3],
          op: orMatch[4],
          value,
          connector: bucket === 0 ? 'AND' : 'OR',
        });
        continue;
      }
      // Top-level AND form: `where[field][op]`.
      const m = key.match(/^where\[([^\]]+)\]\[([^\]]+)\]$/);
      if (m) filters.push({ field: m[1], op: m[2], value, connector: 'AND' });
    }
    return filters;
  }

  /**
   * Apply rows: rebuild the URL's `where[…]` params, reset to page 1,
   * navigate via HTMX (or full reload if HTMX is absent). Emits a
   * bubbling `crap:filter-builder-applied` event that the orchestrator
   * listens for to close the surrounding drawer.
   *
   * @param {HTMLElement} rowsEl
   */
  _applyFilters(rowsEl) {
    this.dispatchEvent(
      new CustomEvent('crap:filter-builder-applied', { bubbles: true, composed: true }),
    );
    navigate(this._buildFilterUrl(rowsEl));
  }

  /**
   * Build the filtered list URL: drop existing `where[…]` params, reset to
   * page 1, then walk the current rows, group adjacent OR-rows into
   * OR-clauses (an AND row breaks the streak), and emit each clause in the
   * URL grammar the server parser understands (`where[field][op]=value` for
   * AND singles, `where[or][G][N][field][op]=value` for OR-clause `G` bucket
   * `N`).
   *
   * @param {HTMLElement} rowsEl
   */
  _buildFilterUrl(rowsEl) {
    const url = new URL(window.location.href);
    // Strip both `where[…]` (rebuilt below from the current rows) AND the
    // pagination cursors. The cursor was issued against the previous
    // result set; with a different filter, the cursor's keyset
    // comparison narrows to empty (or wrong-position rows). Resetting to
    // page=1 gives a fresh query against the new filter.
    for (const key of [...url.searchParams.keys()]) {
      if (key.startsWith('where[') || key === 'after_cursor' || key === 'before_cursor') {
        url.searchParams.delete(key);
      }
    }
    url.searchParams.set('page', '1');

    // Walk rows top-to-bottom, accumulating filters into a buffer that
    // flushes whenever an AND row breaks the streak. Buffer of size 1 →
    // top-level Single; buffer of size 2+ → an OR-clause with one bucket
    // per filter.
    const filters = this._collectFilters(rowsEl);
    /** @type {Array<{ kind: 'and', f: Filter } | { kind: 'or', filters: Filter[] }>} */
    const clauses = [];
    /** @type {Filter[]} */
    let buffer = [];
    const flush = () => {
      if (buffer.length === 1) {
        clauses.push({ kind: 'and', f: buffer[0] });
      } else if (buffer.length > 1) {
        clauses.push({ kind: 'or', filters: buffer.slice() });
      }
      buffer = [];
    };
    for (let i = 0; i < filters.length; i++) {
      const f = filters[i];
      // First row's connector is irrelevant (no prev row to connect to);
      // treat it as AND so it starts the buffer cleanly.
      const isOr = i > 0 && f.connector === 'OR';
      if (!isOr) flush();
      buffer.push(f);
    }
    flush();

    let orGroupIdx = 0;
    for (const clause of clauses) {
      if (clause.kind === 'and') {
        url.searchParams.append(`where[${clause.f.field}][${clause.f.op}]`, clause.f.value);
      } else {
        clause.filters.forEach((f, bucketIdx) => {
          url.searchParams.append(
            `where[or][${orGroupIdx}][${bucketIdx}][${f.field}][${f.op}]`,
            f.value,
          );
        });
        orGroupIdx++;
      }
    }
    return url;
  }

  /**
   * Read the current value of every filter row in `rowsEl`. Captures the
   * connector ('AND'/'OR') alongside field/op/value so `_buildFilterUrl` can
   * group adjacent OR rows into OR-clauses.
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
      const connectorEl = /** @type {HTMLSelectElement|null} */ (
        row.querySelector('.filter-builder__connector')
      );
      const field = fieldEl.value;
      const op = opEl.value;
      const value = valueEl?.value || '';
      const connector = connectorEl?.value === 'OR' ? 'OR' : 'AND';
      // Skip rows with no field or op (defensive — shouldn't happen with
      // the current builder, but a row that somehow lost its selects
      // should not produce a `where[][]=` entry). Skip rows with an
      // empty value too, except for `exists` / `not_exists` which take
      // no value by definition.
      if (!field || !op) continue;
      const valueless = op === 'exists' || op === 'not_exists';
      if (!valueless && value === '') continue;
      filters.push({ field, op, value, connector });
    }
    return filters;
  }
}

if (!customElements.get('crap-filter-builder')) {
  customElements.define('crap-filter-builder', CrapFilterBuilder);
}
