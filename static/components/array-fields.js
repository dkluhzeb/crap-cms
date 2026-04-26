/**
 * Array and blocks field repeater — `<crap-array-field>`.
 *
 * Handles add/remove/reorder/duplicate rows, drag-and-drop sorting,
 * index rewriting, live row label watchers, empty state, and max_rows.
 *
 * @module array-fields
 */

/**
 * Kinds of reference sites a row contains. Each call to {@link rewriteRefs}
 * dispatches the rewriter with one of these tags so callers can apply
 * different rules per attribute kind without duplicating the DOM walk.
 *
 * @typedef {'rowTitle'|'name'|'fieldName'|'id'|'labelFor'|'tplId'|'fieldNameAttr'} RefKind
 */

/**
 * @callback RefRewriter
 * @param {string} value Current attribute value.
 * @param {RefKind} kind Which kind of reference site this is. Return `value`
 *   unchanged to skip a kind.
 * @returns {string}
 */

/** @param {string} str */
const escapeRegex = (str) => str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/**
 * Mirror of Rust `safe_template_id`: `[` → `-`, `]` removed.
 * @param {string} name
 */
const safeName = (name) => name.replace(/\[/g, '-').replace(/\]/g, '');

/**
 * Walk a subtree applying `rewriter` to every site that may carry a row
 * index reference. Optionally recurses into nested `<template>` content.
 *
 * Centralises the DOM walk shared by initial row materialisation and
 * post-reorder reindexing — both rewrite the same attribute set, only
 * the regex/string substitution differs.
 *
 * @param {Element|DocumentFragment} root
 * @param {RefRewriter} rewriter
 * @param {boolean} [recurseTemplates=true]
 */
function rewriteRefs(root, rewriter, recurseTemplates = true) {
  for (const el of root.querySelectorAll('.form__array-row-title')) {
    if (el.textContent) el.textContent = rewriter(el.textContent, 'rowTitle');
  }
  for (const input of /** @type {NodeListOf<HTMLInputElement>} */ (
    root.querySelectorAll('input, select, textarea')
  )) {
    if (input.name) input.name = rewriter(input.name, 'name');
  }
  for (const el of root.querySelectorAll('[data-field-name]')) {
    const v = el.getAttribute('data-field-name');
    if (v) el.setAttribute('data-field-name', rewriter(v, 'fieldName'));
  }
  for (const el of root.querySelectorAll('[id]')) {
    if (el.id) el.id = rewriter(el.id, 'id');
  }
  for (const el of root.querySelectorAll('label[for]')) {
    const v = el.getAttribute('for');
    if (v) el.setAttribute('for', rewriter(v, 'labelFor'));
  }
  for (const el of root.querySelectorAll('[data-template-id]')) {
    const v = el.getAttribute('data-template-id');
    if (v) el.setAttribute('data-template-id', rewriter(v, 'tplId'));
  }
  for (const el of root.querySelectorAll('[field-name]')) {
    const v = el.getAttribute('field-name');
    if (v) el.setAttribute('field-name', rewriter(v, 'fieldNameAttr'));
  }

  if (recurseTemplates) {
    for (const tmpl of /** @type {NodeListOf<HTMLTemplateElement>} */ (
      root.querySelectorAll('template')
    )) {
      rewriteRefs(tmpl.content, rewriter, true);
    }
  }
}

class CrapArrayField extends HTMLElement {
  constructor() {
    super();
    /** @type {HTMLElement|null} */
    this._draggedRow = null;
    /** @type {boolean} */
    this._connected = false;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    this.addEventListener('click', this._onClick.bind(this));
    this.addEventListener('dragstart', this._onDragStart.bind(this));
    this.addEventListener('dragend', this._onDragEnd.bind(this));
    this.addEventListener('dragover', this._onDragOver.bind(this));
    this.addEventListener('drop', this._onDrop.bind(this));
    this.addEventListener('crap:request-add-block', /** @param {Event} e */(e) => {
      const ce = /** @type {CustomEvent} */ (e);
      if (/** @type {HTMLElement} */ (ce.target).closest('crap-array-field') !== this) return;
      this._addBlockRow(ce.detail.templateId);
    });
    this._initLabelWatchers();
  }

  disconnectedCallback() {
    // Do NOT reset _connected — listeners on `this` survive disconnect and must
    // not be re-added on reconnect (row moves via insertBefore trigger
    // disconnect→reconnect on nested custom elements, which would duplicate handlers).
    if (this._draggedRow) {
      this._draggedRow.classList.remove('form__array-row--dragging');
      this._draggedRow = null;
    }
  }

  /* ── Click delegation ──────────────────────────────────────── */

  /** @param {MouseEvent} e */
  _onClick(e) {
    const el = /** @type {HTMLElement|null} */ (
      /** @type {HTMLElement} */ (e.target).closest('[data-action]')
    );
    if (!el || !this.contains(el)) return;
    if (el.closest('crap-array-field') !== this) return;

    switch (el.dataset.action) {
      case 'toggle-array-row': this._toggleRow(el); break;
      case 'toggle-all-rows': this._toggleAllRows(el); break;
      case 'move-row-up': this._moveRowUp(el); break;
      case 'move-row-down': this._moveRowDown(el); break;
      case 'duplicate-row': this._duplicateRow(el); break;
      case 'remove-array-row': this._removeRow(el); break;
      case 'add-array-row': this._addArrayRow(el.dataset.templateId || ''); break;
      case 'noop': break;
    }
  }

  /* ── Row label watchers ────────────────────────────────────── */

  _initLabelWatchers() {
    const fs = this._fieldset;
    if (!fs) return;
    const labelField = fs.getAttribute('data-label-field');
    if (!labelField) return;
    for (const row of /** @type {NodeListOf<HTMLElement>} */ (
      fs.querySelectorAll(':scope > .form__array-rows > .form__array-row')
    )) {
      this._setupRowLabelWatcher(row, labelField);
    }
  }

  /**
   * @param {HTMLElement} row
   * @param {string} labelFieldName
   */
  _setupRowLabelWatcher(row, labelFieldName) {
    if (row.dataset.labelInit) return;
    row.dataset.labelInit = '1';
    const titleEl = row.querySelector('.form__array-row-title');
    if (!titleEl) return;
    const suffix = `[${labelFieldName}]`;
    for (const input of /** @type {NodeListOf<HTMLInputElement>} */ (
      row.querySelectorAll('input, select, textarea')
    )) {
      if (!input.name?.endsWith(suffix)) continue;
      input.addEventListener('input', () => {
        if (input.value) titleEl.textContent = input.value;
      });
      break;
    }
  }

  /* ── State helpers ─────────────────────────────────────────── */

  get _fieldset() {
    return /** @type {HTMLElement|null} */ (this.querySelector('.form__array'));
  }

  /**
   * Whether this fieldset is at its `data-max-rows` cap. Returns `false`
   * when no cap is set, the cap is non-numeric, or required nodes are missing.
   */
  _isAtMax() {
    const fs = this._fieldset;
    if (!fs) return false;
    const addBtn = /** @type {HTMLElement|null} */ (fs.querySelector('[data-max-rows]'));
    if (!addBtn?.dataset.maxRows) return false;
    const max = parseInt(addBtn.dataset.maxRows, 10);
    if (!Number.isFinite(max)) return false;
    const container = fs.querySelector('.form__array-rows');
    return !!container && container.children.length >= max;
  }

  _updateRowCount() {
    const fs = this._fieldset;
    if (!fs) return;
    const container = fs.querySelector('.form__array-rows');
    const rowsEl = fs.querySelector('[id^="array-rows-"]');
    const templateId = rowsEl?.id?.replace('array-rows-', '');
    if (!templateId || !container) return;
    const badge = this.querySelector('#array-count-' + templateId);
    if (badge) badge.textContent = String(container.children.length);
  }

  _toggleEmptyState() {
    const fs = this._fieldset;
    if (!fs) return;
    const container = fs.querySelector('.form__array-rows');
    const empty = /** @type {HTMLElement|null} */ (fs.querySelector('.form__array-empty'));
    if (!container || !empty) return;
    empty.hidden = container.children.length > 0;
  }

  _enforceMaxRows() {
    const fs = this._fieldset;
    if (!fs) return;
    const addBtn = /** @type {HTMLButtonElement|null} */ (fs.querySelector('[data-max-rows]'));
    if (addBtn) addBtn.disabled = this._isAtMax();
  }

  _afterRowChange() {
    this._reindexRows();
    this._updateRowCount();
    this._toggleEmptyState();
    this._enforceMaxRows();
  }

  /* ── Index rewriting ───────────────────────────────────────── */

  /**
   * Materialise a freshly cloned template row at `index`:
   *  - replace `__INDEX__` literally everywhere in the outer row, and
   *  - inside nested `<template>` content, replace only the *parent* field's
   *    `__INDEX__` so child-level placeholders survive for when child rows
   *    are added later.
   *
   * @param {HTMLElement} html
   * @param {number} index
   */
  _replaceTemplateIndex(html, index) {
    html.setAttribute('data-row-index', String(index));

    const replaceIdx = /** @type {RefRewriter} */ (
      (s) => s.replaceAll('__INDEX__', String(index))
    );
    rewriteRefs(html, replaceIdx, false);

    const fieldName = this._fieldset?.getAttribute('data-field-name') || '';
    if (!fieldName) return;

    const bracket = `${fieldName}[__INDEX__]`;
    const bracketRepl = `${fieldName}[${index}]`;
    const dash = `${safeName(fieldName)}-__INDEX__`;
    const dashRepl = `${safeName(fieldName)}-${index}`;

    /** @type {RefRewriter} */
    const parentReplacer = (s, kind) => {
      switch (kind) {
        case 'name':
        case 'fieldName':
          return s.replaceAll(bracket, bracketRepl);
        case 'tplId':
          return s.replaceAll(dash, dashRepl);
        case 'id':
        case 'labelFor':
          return s.replaceAll(bracket, bracketRepl).replaceAll(dash, dashRepl);
        default:
          return s;
      }
    };

    for (const tmpl of /** @type {NodeListOf<HTMLTemplateElement>} */ (
      html.querySelectorAll('template')
    )) {
      rewriteRefs(tmpl.content, parentReplacer, true);
    }
  }

  /**
   * Re-number rows after add/remove/reorder. Walks each row's outer DOM
   * and any nested template content, swapping the existing numeric index
   * for the row's new position.
   */
  _reindexRows() {
    const fs = this._fieldset;
    if (!fs) return;
    const fieldName = fs.getAttribute('data-field-name') || '';
    const container = fs.querySelector('.form__array-rows');
    if (!container || !fieldName) return;

    const safe = safeName(fieldName);
    const bracketPat = new RegExp(`(${escapeRegex(fieldName)}\\[)\\d+(\\])`);
    const idBracketPat = new RegExp(`(field-${escapeRegex(fieldName)}\\[)\\d+(\\])`);
    const dashPat = new RegExp(`(${escapeRegex(safe)}-)\\d+`);

    [...container.children].forEach((child, idx) => {
      child.setAttribute('data-row-index', String(idx));

      /** @type {RefRewriter} */
      const replacer = (s, kind) => {
        switch (kind) {
          case 'name':
          case 'fieldName':
            return s.replace(bracketPat, `$1${idx}$2`);
          case 'tplId':
            return s.replace(dashPat, `$1${idx}`);
          case 'id':
          case 'labelFor':
            return s.replace(idBracketPat, `$1${idx}$2`).replace(dashPat, `$1${idx}`);
          default:
            return s;
        }
      };

      rewriteRefs(child, replacer, true);
    });
  }

  /* ── Row actions ────────────────────────────────────────────── */

  /** @param {HTMLElement} header */
  _toggleRow(header) {
    const row = header.closest('.form__array-row');
    if (!row) return;
    row.classList.toggle('form__array-row--collapsed');
    const collapsed = row.classList.contains('form__array-row--collapsed');
    const toggleBtn = row.querySelector('.form__array-row-toggle');
    if (toggleBtn) toggleBtn.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
  }

  /** @param {HTMLElement} btn */
  _toggleAllRows(btn) {
    const fs = btn.closest('.form__array');
    if (!fs) return;
    const rows = fs.querySelectorAll(':scope > .form__array-rows > .form__array-row');
    const anyExpanded = [...rows].some((r) => !r.classList.contains('form__array-row--collapsed'));
    for (const row of rows) {
      row.classList.toggle('form__array-row--collapsed', anyExpanded);
      const toggleBtn = row.querySelector('.form__array-row-toggle');
      if (toggleBtn) toggleBtn.setAttribute('aria-expanded', anyExpanded ? 'false' : 'true');
    }
    const icon = btn.querySelector('.material-symbols-outlined');
    if (icon) icon.textContent = anyExpanded ? 'unfold_more' : 'unfold_less';
  }

  /** @param {HTMLElement} btn */
  _moveRowUp(btn) {
    const row = btn.closest('.form__array-row');
    if (!row?.previousElementSibling || !row.parentElement) return;
    row.parentElement.insertBefore(row, row.previousElementSibling);
    this._reindexRows();
  }

  /** @param {HTMLElement} btn */
  _moveRowDown(btn) {
    const row = btn.closest('.form__array-row');
    if (!row?.nextElementSibling || !row.parentElement) return;
    row.parentElement.insertBefore(row.nextElementSibling, row);
    this._reindexRows();
  }

  /** @param {HTMLElement} btn */
  _duplicateRow(btn) {
    const row = btn.closest('.form__array-row');
    if (!row) return;
    if (this._isAtMax()) return;

    const clone = /** @type {HTMLElement} */ (row.cloneNode(true));
    delete clone.dataset.labelInit;
    row.after(clone);
    this._initClonedSubtree(clone);
    this._afterRowChange();
  }

  /** @param {HTMLElement} btn */
  _removeRow(btn) {
    const row = btn.closest('.form__array-row');
    if (!row) return;
    row.remove();
    this._afterRowChange();
  }

  /** @param {string} templateId */
  _addArrayRow(templateId) {
    const template = /** @type {HTMLTemplateElement|null} */ (
      this.querySelector(`#array-template-${templateId}`)
    );
    if (template) this._addRow(template);
  }

  /** @param {string} templateId */
  _addBlockRow(templateId) {
    const typeSelect = /** @type {HTMLSelectElement|null} */ (
      this.querySelector(`#block-type-${templateId}`)
    );
    if (!typeSelect) return;
    const template = /** @type {HTMLTemplateElement|null} */ (
      this.querySelector(`#block-template-${templateId}-${typeSelect.value}`)
    );
    if (!template) return;
    this._addRow(template, template.getAttribute('data-label-field') || undefined);
  }

  /**
   * Append a clone of `template` to the rows container at the next index,
   * then wire up richtext / label watchers.
   *
   * @param {HTMLTemplateElement} template
   * @param {string} [labelOverride] block-specific label field, falling back
   *   to the fieldset-level `data-label-field`.
   */
  _addRow(template, labelOverride) {
    const fs = this._fieldset;
    if (!fs) return;
    if (this._isAtMax()) return;
    const container = fs.querySelector('.form__array-rows');
    if (!container) return;

    const nextIndex = container.children.length;
    const clone = template.content.cloneNode(true);
    const html = /** @type {HTMLElement|null} */ (
      /** @type {DocumentFragment} */ (clone).firstElementChild
    );
    if (html) this._replaceTemplateIndex(html, nextIndex);

    container.appendChild(clone);
    if (html) this._initClonedSubtree(html, labelOverride);
    this._afterRowChange();
  }

  /**
   * Re-run `connectedCallback` on richtext components inside a freshly
   * inserted clone (cloneNode does not fire it) and install the label
   * watcher.
   *
   * @param {HTMLElement} html
   * @param {string} [labelOverride]
   */
  _initClonedSubtree(html, labelOverride) {
    for (const el of /** @type {NodeListOf<HTMLElement & {connectedCallback?: () => void}>} */ (
      html.querySelectorAll('crap-richtext')
    )) {
      el.connectedCallback?.();
    }
    const labelField = labelOverride || this._fieldset?.getAttribute('data-label-field');
    if (labelField) this._setupRowLabelWatcher(html, labelField);
  }

  /* ── Drag-and-drop ─────────────────────────────────────────── */

  /**
   * Resolve the rows container an event targets, scoped to *this* fieldset
   * (drag events from a nested `<crap-array-field>` are ignored).
   *
   * @param {EventTarget|null} target
   * @returns {HTMLElement|null}
   */
  _findRowsContainer(target) {
    if (!(target instanceof Element)) return null;
    const container = /** @type {HTMLElement|null} */ (target.closest('.form__array-rows'));
    if (!container || container.closest('.form__array') !== this._fieldset) return null;
    return container;
  }

  /** @param {DragEvent} e */
  _onDragStart(e) {
    const handle = /** @type {HTMLElement|null} */ (
      e.target instanceof Element ? e.target.closest('[draggable][data-drag]') : null
    );
    if (!handle || handle.closest('crap-array-field') !== this) return;
    this._draggedRow = handle.closest('.form__array-row');
    if (!this._draggedRow || !e.dataTransfer) return;
    this._draggedRow.classList.add('form__array-row--dragging');
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', '');
  }

  _onDragEnd() {
    if (this._draggedRow) {
      this._draggedRow.classList.remove('form__array-row--dragging');
      this._draggedRow = null;
    }
    this._clearDragOver(this);
  }

  /** @param {DragEvent} e */
  _onDragOver(e) {
    const container = this._findRowsContainer(e.target);
    if (!container) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
    if (!this._draggedRow) return;
    const afterEl = this._getDragAfterElement(container, e.clientY);
    this._clearDragOver(container);
    if (afterEl) afterEl.classList.add('form__array-row--drag-over');
  }

  /** @param {DragEvent} e */
  _onDrop(e) {
    const container = this._findRowsContainer(e.target);
    if (!container) return;
    e.preventDefault();
    if (!this._draggedRow) return;
    const afterEl = this._getDragAfterElement(container, e.clientY);
    if (afterEl) {
      container.insertBefore(this._draggedRow, afterEl);
    } else {
      container.appendChild(this._draggedRow);
    }
    this._clearDragOver(container);
    this._reindexRows();
  }

  /** @param {Element} root */
  _clearDragOver(root) {
    for (const el of root.querySelectorAll('.form__array-row--drag-over')) {
      el.classList.remove('form__array-row--drag-over');
    }
  }

  /**
   * Find the row immediately below the cursor — the one a drop would
   * insert *before*. Returns `null` to mean "append to end".
   *
   * @param {HTMLElement} container
   * @param {number} y
   * @returns {HTMLElement|null}
   */
  _getDragAfterElement(container, y) {
    const rows = /** @type {HTMLElement[]} */ ([
      ...container.querySelectorAll(':scope > .form__array-row:not(.form__array-row--dragging)'),
    ]);
    let closestOffset = Number.NEGATIVE_INFINITY;
    /** @type {HTMLElement|null} */
    let closestEl = null;
    for (const row of rows) {
      const box = row.getBoundingClientRect();
      const offset = y - box.top - box.height / 2;
      if (offset < 0 && offset > closestOffset) {
        closestOffset = offset;
        closestEl = row;
      }
    }
    return closestEl;
  }
}

customElements.define('crap-array-field', CrapArrayField);
