/**
 * Array and blocks field repeater — `<crap-array-field>`.
 *
 * Handles add/remove/reorder/duplicate rows, drag-and-drop sorting,
 * index rewriting, live row label watchers, empty state, and max_rows.
 *
 * @module array-fields
 */

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
    this.addEventListener('crap:request-add-block', /** @param {CustomEvent} e */ (e) => {
      if (/** @type {HTMLElement} */ (e.target).closest('crap-array-field') !== this) return;
      this._addBlockRow(e.detail.templateId);
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
    const el = /** @type {HTMLElement} */ (e.target).closest('[data-action]');
    if (!el || !this.contains(el)) return;
    if (el.closest('crap-array-field') !== this) return;

    switch (/** @type {HTMLElement} */ (el).dataset.action) {
      case 'toggle-array-row': this._toggleRow(el); break;
      case 'toggle-all-rows': this._toggleAllRows(el); break;
      case 'move-row-up': this._moveRowUp(el); break;
      case 'move-row-down': this._moveRowDown(el); break;
      case 'duplicate-row': this._duplicateRow(el); break;
      case 'remove-array-row': this._removeRow(el); break;
      case 'add-array-row': this._addArrayRow(/** @type {HTMLElement} */ (el).dataset.templateId); break;
      case 'noop': break;
    }
  }

  /* ── Row label watchers ────────────────────────────────────── */

  _initLabelWatchers() {
    const fs = this._fieldset;
    if (!fs) return;
    const labelField = fs.getAttribute('data-label-field');
    if (!labelField) return;
    fs.querySelectorAll(':scope > .form__array-rows > .form__array-row').forEach(
      /** @param {HTMLElement} row */ (row) => this._setupRowLabelWatcher(row, labelField)
    );
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
    for (const input of row.querySelectorAll('input, select, textarea')) {
      if (/** @type {HTMLInputElement} */ (input).name?.endsWith('[' + labelFieldName + ']')) {
        input.addEventListener('input', () => {
          const val = /** @type {HTMLInputElement} */ (input).value;
          if (val) titleEl.textContent = val;
        });
        break;
      }
    }
  }

  /* ── Helpers ────────────────────────────────────────────────── */

  get _fieldset() {
    return this.querySelector('.form__array');
  }

  /** @param {string} str @returns {string} */
  _escapeRegex(str) {
    return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  }

  /**
   * Convert a field name to a safe template ID (mirrors Rust safe_template_id).
   * Replaces `[` with `-` and removes `]`.
   *
   * @param {string} name
   * @returns {string}
   */
  _safeName(name) {
    return name.replace(/\[/g, '-').replace(/\]/g, '');
  }

  /**
   * @param {Element|DocumentFragment} root
   * @param {number} index
   */
  _replaceIndexInSubtree(root, index) {
    root.querySelectorAll('.form__array-row-title').forEach(
      /** @param {HTMLElement} el */ (el) => {
        if (el.textContent?.includes('__INDEX__')) {
          el.textContent = el.textContent.replaceAll('__INDEX__', String(index));
        }
      }
    );
    root.querySelectorAll('input, select, textarea').forEach(
      /** @param {HTMLInputElement} input */ (input) => {
        if (input.name) input.name = input.name.replaceAll('__INDEX__', String(index));
      }
    );
    root.querySelectorAll('[data-field-name*="__INDEX__"]').forEach(
      /** @param {HTMLElement} el */ (el) => {
        const fn = el.getAttribute('data-field-name');
        if (fn) el.setAttribute('data-field-name', fn.replaceAll('__INDEX__', String(index)));
      }
    );
    root.querySelectorAll('[id*="__INDEX__"]').forEach(
      /** @param {HTMLElement} el */ (el) => {
        el.id = el.id.replaceAll('__INDEX__', String(index));
      }
    );
    root.querySelectorAll('label[for*="__INDEX__"]').forEach(
      /** @param {HTMLLabelElement} el */ (el) => {
        el.setAttribute('for', el.getAttribute('for').replaceAll('__INDEX__', String(index)));
      }
    );
    root.querySelectorAll('[data-template-id*="__INDEX__"]').forEach(
      /** @param {HTMLElement} el */ (el) => {
        const tid = el.getAttribute('data-template-id');
        if (tid) el.setAttribute('data-template-id', tid.replaceAll('__INDEX__', String(index)));
      }
    );
  }

  /**
   * Replace parent-level __INDEX__ in nested <template> elements using targeted patterns.
   * Only replaces occurrences that match the parent field name, preserving child-level placeholders.
   *
   * @param {Element|DocumentFragment} root
   * @param {number} index
   */
  _replaceIndexInNestedTemplates(root, index) {
    const fs = this._fieldset;
    if (!fs) return;
    const fieldName = fs.getAttribute('data-field-name') || '';
    if (!fieldName) return;

    const bracketSearch = fieldName + '[__INDEX__]';
    const bracketReplace = fieldName + '[' + index + ']';
    const safeName = this._safeName(fieldName);
    const dashSearch = safeName + '-__INDEX__';
    const dashReplace = safeName + '-' + index;

    this._replaceTargetedInTemplates(root, bracketSearch, bracketReplace, dashSearch, dashReplace);
  }

  /**
   * Recursively apply targeted parent-level index replacement inside nested <template> elements.
   *
   * @param {Element|DocumentFragment} root
   * @param {string} bracketSearch  — e.g. "main_nav[__INDEX__]"
   * @param {string} bracketReplace — e.g. "main_nav[0]"
   * @param {string} dashSearch     — e.g. "main_nav-__INDEX__"
   * @param {string} dashReplace    — e.g. "main_nav-0"
   */
  _replaceTargetedInTemplates(root, bracketSearch, bracketReplace, dashSearch, dashReplace) {
    root.querySelectorAll('template').forEach(
      /** @param {HTMLTemplateElement} tmpl */ (tmpl) => {
        const c = tmpl.content;

        c.querySelectorAll('input, select, textarea').forEach(
          /** @param {HTMLInputElement} input */ (input) => {
            if (input.name) input.name = input.name.replaceAll(bracketSearch, bracketReplace);
          }
        );

        c.querySelectorAll('[data-field-name]').forEach(
          /** @param {HTMLElement} el */ (el) => {
            const fn = el.getAttribute('data-field-name');
            if (fn) el.setAttribute('data-field-name', fn.replaceAll(bracketSearch, bracketReplace));
          }
        );

        c.querySelectorAll('[id]').forEach(
          /** @param {HTMLElement} el */ (el) => {
            el.id = el.id
              .replaceAll(bracketSearch, bracketReplace)
              .replaceAll(dashSearch, dashReplace);
          }
        );

        c.querySelectorAll('label[for]').forEach(
          /** @param {HTMLLabelElement} el */ (el) => {
            const f = el.getAttribute('for');
            if (f) el.setAttribute('for', f
              .replaceAll(bracketSearch, bracketReplace)
              .replaceAll(dashSearch, dashReplace));
          }
        );

        c.querySelectorAll('[data-template-id]').forEach(
          /** @param {HTMLElement} el */ (el) => {
            const tid = el.getAttribute('data-template-id');
            if (tid) el.setAttribute('data-template-id', tid.replaceAll(dashSearch, dashReplace));
          }
        );

        this._replaceTargetedInTemplates(c, bracketSearch, bracketReplace, dashSearch, dashReplace);
      }
    );
  }

  /**
   * @param {HTMLElement} html
   * @param {number} index
   */
  _replaceTemplateIndex(html, index) {
    html.setAttribute('data-row-index', String(index));
    this._replaceIndexInSubtree(html, index);
    this._replaceIndexInNestedTemplates(html, index);
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
    empty.style.display = container.children.length === 0 ? '' : 'none';
  }

  _enforceMaxRows() {
    const fs = this._fieldset;
    if (!fs) return;
    const addBtn = /** @type {HTMLButtonElement|null} */ (fs.querySelector('[data-max-rows]'));
    if (!addBtn) return;
    const max = parseInt(/** @type {HTMLElement} */ (addBtn).dataset.maxRows, 10);
    const container = fs.querySelector('.form__array-rows');
    if (container) addBtn.disabled = container.children.length >= max;
  }

  _reindexRows() {
    const fs = this._fieldset;
    if (!fs) return;
    const fieldName = fs.getAttribute('data-field-name') || '';
    const container = fs.querySelector('.form__array-rows');
    if (!container || !fieldName) return;

    const bracketPattern = new RegExp('(' + this._escapeRegex(fieldName) + '\\[)\\d+(\\])');
    const idBracketPattern = new RegExp('(field-' + this._escapeRegex(fieldName) + '\\[)\\d+(\\])');
    const safeName = this._safeName(fieldName);
    const dashPattern = new RegExp('(' + this._escapeRegex(safeName) + '-)\\d+');

    Array.from(container.children).forEach(
      /** @param {Element} child @param {number} idx */
      (child, idx) => {
        child.setAttribute('data-row-index', String(idx));

        child.querySelectorAll('input, select, textarea').forEach(
          /** @param {HTMLInputElement} input */ (input) => {
            if (input.name) input.name = input.name.replace(bracketPattern, `$1${idx}$2`);
          }
        );

        child.querySelectorAll('[data-field-name]').forEach(
          /** @param {HTMLElement} el */ (el) => {
            const fn = el.getAttribute('data-field-name');
            if (fn) el.setAttribute('data-field-name', fn.replace(bracketPattern, `$1${idx}$2`));
          }
        );

        child.querySelectorAll('[id]').forEach(
          /** @param {HTMLElement} el */ (el) => {
            el.id = el.id
              .replace(idBracketPattern, `$1${idx}$2`)
              .replace(dashPattern, `$1${idx}`);
          }
        );

        child.querySelectorAll('label[for]').forEach(
          /** @param {HTMLLabelElement} el */ (el) => {
            const f = el.getAttribute('for');
            if (f) el.setAttribute('for', f
              .replace(idBracketPattern, `$1${idx}$2`)
              .replace(dashPattern, `$1${idx}`));
          }
        );

        child.querySelectorAll('[data-template-id]').forEach(
          /** @param {HTMLElement} el */ (el) => {
            const tid = el.getAttribute('data-template-id');
            if (tid) el.setAttribute('data-template-id', tid.replace(dashPattern, `$1${idx}`));
          }
        );

        this._reindexNestedTemplates(child, bracketPattern, idBracketPattern, dashPattern, idx);
      }
    );
  }

  /**
   * Reindex nested <template> content after parent row reorder.
   *
   * @param {Element|DocumentFragment} root
   * @param {RegExp} bracketPattern
   * @param {RegExp} idBracketPattern
   * @param {RegExp} dashPattern
   * @param {number} idx
   */
  _reindexNestedTemplates(root, bracketPattern, idBracketPattern, dashPattern, idx) {
    root.querySelectorAll('template').forEach(
      /** @param {HTMLTemplateElement} tmpl */ (tmpl) => {
        const c = tmpl.content;

        c.querySelectorAll('input, select, textarea').forEach(
          /** @param {HTMLInputElement} input */ (input) => {
            if (input.name) input.name = input.name.replace(bracketPattern, `$1${idx}$2`);
          }
        );

        c.querySelectorAll('[data-field-name]').forEach(
          /** @param {HTMLElement} el */ (el) => {
            const fn = el.getAttribute('data-field-name');
            if (fn) el.setAttribute('data-field-name', fn.replace(bracketPattern, `$1${idx}$2`));
          }
        );

        c.querySelectorAll('[id]').forEach(
          /** @param {HTMLElement} el */ (el) => {
            el.id = el.id
              .replace(idBracketPattern, `$1${idx}$2`)
              .replace(dashPattern, `$1${idx}`);
          }
        );

        c.querySelectorAll('label[for]').forEach(
          /** @param {HTMLLabelElement} el */ (el) => {
            const f = el.getAttribute('for');
            if (f) el.setAttribute('for', f
              .replace(idBracketPattern, `$1${idx}$2`)
              .replace(dashPattern, `$1${idx}`));
          }
        );

        c.querySelectorAll('[data-template-id]').forEach(
          /** @param {HTMLElement} el */ (el) => {
            const tid = el.getAttribute('data-template-id');
            if (tid) el.setAttribute('data-template-id', tid.replace(dashPattern, `$1${idx}`));
          }
        );

        this._reindexNestedTemplates(c, bracketPattern, idBracketPattern, dashPattern, idx);
      }
    );
  }

  _afterRowChange() {
    this._reindexRows();
    this._updateRowCount();
    this._toggleEmptyState();
    this._enforceMaxRows();
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
    rows.forEach(/** @param {HTMLElement} row */ (row) => {
      row.classList.toggle('form__array-row--collapsed', anyExpanded);
      const toggleBtn = row.querySelector('.form__array-row-toggle');
      if (toggleBtn) toggleBtn.setAttribute('aria-expanded', anyExpanded ? 'false' : 'true');
    });
    const icon = btn.querySelector('.material-symbols-outlined');
    if (icon) icon.textContent = anyExpanded ? 'unfold_more' : 'unfold_less';
  }

  /** @param {HTMLElement} btn */
  _moveRowUp(btn) {
    const row = btn.closest('.form__array-row');
    if (!row?.previousElementSibling) return;
    row.parentElement.insertBefore(row, row.previousElementSibling);
    this._reindexRows();
  }

  /** @param {HTMLElement} btn */
  _moveRowDown(btn) {
    const row = btn.closest('.form__array-row');
    if (!row?.nextElementSibling) return;
    row.parentElement.insertBefore(row.nextElementSibling, row);
    this._reindexRows();
  }

  /** @param {HTMLElement} btn */
  _duplicateRow(btn) {
    const row = btn.closest('.form__array-row');
    if (!row) return;
    const fs = this._fieldset;

    if (fs) {
      const addBtn = /** @type {HTMLElement|null} */ (fs.querySelector('[data-max-rows]'));
      if (addBtn) {
        const max = parseInt(addBtn.dataset.maxRows, 10);
        const container = fs.querySelector('.form__array-rows');
        if (container && container.children.length >= max) return;
      }
    }

    const clone = /** @type {HTMLElement} */ (row.cloneNode(true));
    delete clone.dataset.labelInit;
    row.after(clone);

    clone.querySelectorAll('crap-richtext').forEach(
      /** @param {HTMLElement} el */ (el) => {
        if (el.connectedCallback) el.connectedCallback();
      }
    );

    if (fs) {
      const labelField = fs.getAttribute('data-label-field');
      if (labelField) this._setupRowLabelWatcher(clone, labelField);
    }

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
    const container = this.querySelector(`#array-rows-${templateId}`);
    if (!template || !container) return;

    const fs = this._fieldset;
    if (fs) {
      const addBtn = /** @type {HTMLElement|null} */ (fs.querySelector('[data-max-rows]'));
      if (addBtn) {
        const max = parseInt(addBtn.dataset.maxRows, 10);
        if (container.children.length >= max) return;
      }
    }

    const nextIndex = container.children.length;
    const clone = template.content.cloneNode(true);
    const html = /** @type {HTMLElement} */ (clone.firstElementChild);
    if (html) this._replaceTemplateIndex(html, nextIndex);

    container.appendChild(clone);
    if (html) {
      html.querySelectorAll('crap-richtext').forEach(
        /** @param {HTMLElement} el */ (el) => {
          if (el.connectedCallback) el.connectedCallback();
        }
      );
      if (fs) {
        const labelField = fs.getAttribute('data-label-field');
        if (labelField) this._setupRowLabelWatcher(html, labelField);
      }
    }
    this._afterRowChange();
  }

  /** @param {string} templateId */
  _addBlockRow(templateId) {
    const typeSelect = /** @type {HTMLSelectElement|null} */ (
      this.querySelector(`#block-type-${templateId}`)
    );
    if (!typeSelect) return;
    const blockType = typeSelect.value;
    const template = /** @type {HTMLTemplateElement|null} */ (
      this.querySelector(`#block-template-${templateId}-${blockType}`)
    );
    const container = this.querySelector(`#array-rows-${templateId}`);
    if (!template || !container) return;

    const fs = this._fieldset;
    if (fs) {
      const addBtn = /** @type {HTMLElement|null} */ (fs.querySelector('[data-max-rows]'));
      if (addBtn) {
        const max = parseInt(addBtn.dataset.maxRows, 10);
        if (container.children.length >= max) return;
      }
    }

    const nextIndex = container.children.length;
    const clone = template.content.cloneNode(true);
    const html = /** @type {HTMLElement} */ (clone.firstElementChild);
    if (html) this._replaceTemplateIndex(html, nextIndex);

    container.appendChild(clone);
    if (html) {
      html.querySelectorAll('crap-richtext').forEach(
        /** @param {HTMLElement} el */ (el) => {
          if (el.connectedCallback) el.connectedCallback();
        }
      );
      if (fs) {
        const blockLabelField = template.getAttribute('data-label-field');
        if (blockLabelField) {
          this._setupRowLabelWatcher(html, blockLabelField);
        } else {
          const labelField = fs.getAttribute('data-label-field');
          if (labelField) this._setupRowLabelWatcher(html, labelField);
        }
      }
    }
    this._afterRowChange();
  }

  /* ── Drag-and-drop ─────────────────────────────────────────── */

  /** @param {DragEvent} e */
  _onDragStart(e) {
    const el = /** @type {HTMLElement} */ (e.target).closest('[draggable][data-drag]');
    if (!el) return;
    if (el.closest('crap-array-field') !== this) return;
    this._draggedRow = el.closest('.form__array-row');
    if (!this._draggedRow) return;
    this._draggedRow.classList.add('form__array-row--dragging');
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', '');
  }

  _onDragEnd() {
    if (this._draggedRow) {
      this._draggedRow.classList.remove('form__array-row--dragging');
      this._draggedRow = null;
    }
    this.querySelectorAll('.form__array-row--drag-over').forEach(
      (el) => el.classList.remove('form__array-row--drag-over')
    );
  }

  /** @param {DragEvent} e */
  _onDragOver(e) {
    const container = /** @type {HTMLElement} */ (e.target).closest('.form__array-rows');
    if (!container || container.closest('.form__array') !== this._fieldset) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    if (!this._draggedRow) return;
    const afterEl = this._getDragAfterElement(container, e.clientY);
    container.querySelectorAll('.form__array-row--drag-over').forEach(
      (el) => el.classList.remove('form__array-row--drag-over')
    );
    if (afterEl) afterEl.classList.add('form__array-row--drag-over');
  }

  /** @param {DragEvent} e */
  _onDrop(e) {
    const container = /** @type {HTMLElement} */ (e.target).closest('.form__array-rows');
    if (!container || container.closest('.form__array') !== this._fieldset) return;
    e.preventDefault();
    if (!this._draggedRow) return;
    const afterEl = this._getDragAfterElement(container, e.clientY);
    if (afterEl) {
      container.insertBefore(this._draggedRow, afterEl);
    } else {
      container.appendChild(this._draggedRow);
    }
    container.querySelectorAll('.form__array-row--drag-over').forEach(
      (el) => el.classList.remove('form__array-row--drag-over')
    );
    this._reindexRows();
  }

  /**
   * @param {HTMLElement} container
   * @param {number} y
   * @returns {HTMLElement|null}
   */
  _getDragAfterElement(container, y) {
    const rows = [...container.querySelectorAll(':scope > .form__array-row:not(.form__array-row--dragging)')];
    return rows.reduce((closest, child) => {
      const box = child.getBoundingClientRect();
      const offset = y - box.top - box.height / 2;
      if (offset < 0 && offset > closest.offset) {
        return { offset, element: child };
      }
      return closest;
    }, { offset: Number.NEGATIVE_INFINITY }).element || null;
  }
}

customElements.define('crap-array-field', CrapArrayField);
