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
 * @typedef {{ field: string, op: string, value: string, connector?: 'AND'|'OR' }} Filter
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
    // htmx 2 renamed the URL-push option from `pushUrl: true` (1.x) to
    // `push: <string>` — the value is either the literal `"true"` or
    // the path to push. `pushUrl` is silently dropped; `push: true`
    // (boolean) gets coerced to the string `"true"` and pushed as the
    // URL `"/.../true"`, which is how the user observed
    // `…/admin/collections/true` after applying. We pass the actual
    // path so history matches what was loaded.
    const path = url.pathname + url.search;
    htmx.ajax('GET', path, { target: 'body', push: path });
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
    const form = this._buildColumnPickerForm(options, slug, drawer);
    drawer.body.appendChild(form);
    // htmx auto-discovery doesn't traverse shadow roots; the drawer's
    // body lives inside `<crap-drawer>`'s shadow DOM, so the new form's
    // `hx-*` attributes are invisible to htmx until we tell it where
    // to look. Without this call, submit goes through the browser's
    // default form action (= the page URL) instead of `hx-post`.
    if (typeof htmx !== 'undefined') htmx.process(form);
  }

  /**
   * @param {ColumnOption[]} options
   * @param {string} slug
   * @param {DrawerInstance} drawer
   */
  _buildColumnPickerForm(options, slug, drawer) {
    // Submission goes through htmx: `hx-post` triggers on form submit,
    // urlencoded body is auto-built from form fields, CSRF is added by
    // the `htmx:configRequest` listener in `templates/layout/base.hbs`,
    // and `hx-swap="none"` tells htmx not to splice the response into
    // the DOM (we just need the success status — the page is reloaded
    // below to pick up the new column selection from the server).
    //
    // We assemble checked column keys into a single hidden `columns`
    // field (the server endpoint expects a comma-joined list, not
    // duplicate `column=` params from `<select multiple>`).
    const list = h(
      'div',
      { class: 'column-picker__list' },
      options.map((opt) =>
        h(
          'label',
          { class: 'column-picker__item' },
          h('input', {
            type: 'checkbox',
            class: 'column-picker__checkbox',
            value: opt.key,
            checked: opt.selected,
          }),
          h('span', { text: t(opt.label) }),
        ),
      ),
    );
    const columnsInput = h('input', { type: 'hidden', name: 'columns', value: '' });
    const updateColumns = () => {
      const checked = /** @type {NodeListOf<HTMLInputElement>} */ (
        list.querySelectorAll('input[type="checkbox"]:checked')
      );
      columnsInput.value = [...checked].map((cb) => cb.value).join(',');
    };
    list.addEventListener('change', updateColumns);
    updateColumns();
    const form = h(
      'form',
      {
        class: 'column-picker',
        'hx-post': `/admin/api/user-settings/${slug}`,
        'hx-swap': 'none',
      },
      columnsInput,
      list,
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
    // Listen for the request outcome on the form itself. htmx's events
    // bubble with `composed: true`, so the form node sees them even
    // though it lives inside `<crap-drawer>`'s shadow root.
    form.addEventListener(
      'htmx:afterRequest',
      /** @param {Event} evt */ (evt) => {
        const detail = /** @type {any} */ (evt).detail;
        if (!detail?.successful) return;
        drawer.close();
        window.location.reload();
      },
    );
    return form;
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
    // No URL filters → drawer opens with zero rows. The "+ Add condition"
    // button below is the affordance for adding the first filter. A
    // null-preset row would auto-hydrate to the first field's first op
    // and first value (typically `_status = "published"` for collections
    // with drafts), so clicking Apply without configuring would silently
    // apply that filter — which is *not* what the user intended.
    const rowsEl = h(
      'div',
      { class: 'filter-builder__rows' },
      presets.map((p) => this._buildFilterRow(fieldMetas, p)),
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
   * Build one filter row: connector + field select + op select + value input
   * + remove. The connector dropdown sits at the front and is hidden on the
   * very first row (no previous row to connect to). The op + value inputs
   * re-render reactively on field/op change.
   *
   * @param {FieldMeta[]} fieldMetas
   * @param {Filter|null} preset
   */
  _buildFilterRow(fieldMetas, preset) {
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
   * Read the current `where[…]` URL params and render them as a flat list of
   * rows ready to feed `_buildFilterRow`. Recognises both the top-level form
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

customElements.define('crap-list-settings', CrapListSettings);
