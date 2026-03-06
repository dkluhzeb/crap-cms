/**
 * Array and blocks field repeater.
 *
 * Handles add/remove/reorder/duplicate rows, drag-and-drop sorting,
 * index rewriting, live row label watchers, empty state, and max_rows.
 *
 * All row actions are registered via the delegation system (actions.js).
 * `addBlockRow` is exported for direct use by block-picker.js.
 */

import { registerAction, registerDrag, registerInit } from './actions.js';

/* ── Row label watchers ──────────────────────────────────────── */

/**
 * Set up a live label watcher on a single array/blocks row.
 *
 * @param {HTMLElement} row - The .form__array-row element.
 * @param {HTMLElement} fieldset - The parent .form__array fieldset.
 */
function setupRowLabelWatcher(row, fieldset) {
  const labelFieldName = fieldset.getAttribute('data-label-field');
  if (!labelFieldName) return;

  const titleEl = row.querySelector('.form__array-row-title');
  if (!titleEl) return;

  const inputs = row.querySelectorAll('input, select, textarea');
  for (const input of inputs) {
    const name = /** @type {HTMLInputElement} */ (input).name || '';
    if (name.endsWith('[' + labelFieldName + ']')) {
      input.addEventListener('input', () => {
        const val = /** @type {HTMLInputElement} */ (input).value;
        if (val) {
          titleEl.textContent = val;
        }
      });
      break;
    }
  }
}

/**
 * Set up a live label watcher on a blocks row using a specific label field name.
 *
 * @param {HTMLElement} row - The .form__array-row element.
 * @param {string} labelFieldName - The sub-field name to watch.
 */
function setupBlockRowLabelWatcher(row, labelFieldName) {
  const titleEl = row.querySelector('.form__array-row-title');
  if (!titleEl) return;

  const inputs = row.querySelectorAll('input, select, textarea');
  for (const input of inputs) {
    const name = /** @type {HTMLInputElement} */ (input).name || '';
    if (name.endsWith('[' + labelFieldName + ']')) {
      input.addEventListener('input', () => {
        const val = /** @type {HTMLInputElement} */ (input).value;
        if (val) {
          titleEl.textContent = val;
        }
      });
      break;
    }
  }
}

/**
 * Initialize row label watchers on all existing array/blocks rows.
 */
function initRowLabelWatchers() {
  document.querySelectorAll('.form__array[data-label-field]').forEach(
    /** @param {HTMLElement} fieldset */ (fieldset) => {
      fieldset.querySelectorAll(':scope > .form__array-rows > .form__array-row').forEach(
        /** @param {HTMLElement} row */ (row) => {
          if (/** @type {HTMLElement} */ (row).dataset.labelInit) return;
          /** @type {HTMLElement} */ (row).dataset.labelInit = '1';
          setupRowLabelWatcher(row, fieldset);
        }
      );
    }
  );
}

registerInit(initRowLabelWatchers);

/* ── Index replacement helpers ───────────────────────────────── */

/**
 * Escape a string for safe use inside a RegExp.
 *
 * @param {string} str
 * @returns {string}
 */
function escapeRegex(str) {
  return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * Replace the first occurrence of `__INDEX__` in relevant attributes
 * within a DOM subtree.
 *
 * @param {Element|DocumentFragment} root
 * @param {number} index
 */
function replaceIndexInSubtree(root, index) {
  root.querySelectorAll('.form__array-row-title').forEach(
    /** @param {HTMLElement} el */ (el) => {
      if (el.textContent && el.textContent.includes('__INDEX__')) {
        el.textContent = el.textContent.replace('__INDEX__', String(index));
      }
    }
  );
  root.querySelectorAll('input, select, textarea').forEach(
    /** @param {HTMLInputElement} input */ (input) => {
      if (input.name) {
        input.name = input.name.replace('__INDEX__', String(index));
      }
    }
  );
  root.querySelectorAll('[data-field-name*="__INDEX__"]').forEach(
    /** @param {HTMLElement} el */ (el) => {
      const fn = el.getAttribute('data-field-name');
      if (fn) {
        el.setAttribute('data-field-name', fn.replace('__INDEX__', String(index)));
      }
    }
  );
  root.querySelectorAll('[id*="__INDEX__"]').forEach(
    /** @param {HTMLElement} el */ (el) => {
      el.id = el.id.replace('__INDEX__', String(index));
    }
  );
  root.querySelectorAll('[data-template-id*="__INDEX__"]').forEach(
    /** @param {HTMLElement} el */ (el) => {
      const tid = el.getAttribute('data-template-id');
      if (tid) {
        el.setAttribute('data-template-id', tid.replace('__INDEX__', String(index)));
      }
    }
  );
}

/**
 * Recursively replace `__INDEX__` inside nested <template> content.
 *
 * @param {Element|DocumentFragment} root
 * @param {number} index
 */
function replaceIndexInNestedTemplates(root, index) {
  root.querySelectorAll('template').forEach(
    /** @param {HTMLTemplateElement} tmpl */ (tmpl) => {
      replaceIndexInSubtree(tmpl.content, index);
      replaceIndexInNestedTemplates(tmpl.content, index);
    }
  );
}

/**
 * Replace `__INDEX__` in a cloned template fragment, including nested templates.
 *
 * @param {HTMLElement} html - The cloned row element
 * @param {number} index
 */
function replaceTemplateIndex(html, index) {
  html.setAttribute('data-row-index', String(index));
  replaceIndexInSubtree(html, index);
  replaceIndexInNestedTemplates(html, index);
}

/* ── Row count & constraints ─────────────────────────────────── */

/**
 * Update the row count badge for an array/blocks fieldset.
 *
 * @param {HTMLElement} fieldset
 */
function updateRowCount(fieldset) {
  const container = fieldset.querySelector('.form__array-rows');
  const templateId = fieldset.querySelector('[id^="array-rows-"]')?.id?.replace('array-rows-', '');
  if (!templateId) return;
  const badge = document.getElementById('array-count-' + templateId);
  if (badge && container) {
    badge.textContent = String(container.children.length);
  }
}

/**
 * Toggle the empty state message visibility.
 *
 * @param {HTMLElement} fieldset
 */
function toggleEmptyState(fieldset) {
  const container = fieldset.querySelector('.form__array-rows');
  const empty = fieldset.querySelector('.form__array-empty');
  if (!container || !empty) return;
  empty.style.display = container.children.length === 0 ? '' : 'none';
}

/**
 * Enforce max_rows constraint by disabling/enabling the add button.
 *
 * @param {HTMLElement} fieldset
 */
function enforceMaxRows(fieldset) {
  const addBtn = fieldset.querySelector('[data-max-rows]');
  if (!addBtn) return;
  const max = parseInt(addBtn.dataset.maxRows, 10);
  const container = fieldset.querySelector('.form__array-rows');
  if (!container) return;
  addBtn.disabled = container.children.length >= max;
}

/**
 * Reindex all rows so form names stay sequential.
 *
 * @param {HTMLElement|null} fieldset
 */
function reindexRows(fieldset) {
  if (!fieldset) return;
  const fieldName = fieldset.getAttribute('data-field-name') || '';
  const container = fieldset.querySelector('.form__array-rows');
  if (!container || !fieldName) return;
  const pattern = new RegExp('(' + escapeRegex(fieldName) + '\\[)\\d+(\\])');
  Array.from(container.children).forEach(
    /** @param {Element} child @param {number} idx */
    (child, idx) => {
      child.setAttribute('data-row-index', String(idx));
      child.querySelectorAll('input, select, textarea').forEach(
        /** @param {HTMLInputElement} input */ (input) => {
          if (input.name) {
            input.name = input.name.replace(pattern, `$1${idx}$2`);
          }
        }
      );
    }
  );
}

/* ── Row actions ─────────────────────────────────────────────── */

/**
 * Toggle a single array row's collapsed state.
 *
 * @param {HTMLElement} header
 */
function toggleArrayRow(header) {
  const row = header.closest('.form__array-row');
  if (!row) return;
  row.classList.toggle('form__array-row--collapsed');
}

/**
 * Toggle all rows in an array/blocks fieldset.
 * If any row is expanded → collapse all (icon → unfold_more).
 * If all collapsed → expand all (icon → unfold_less).
 *
 * @param {HTMLButtonElement} btn
 */
function toggleAllRows(btn) {
  const fieldset = btn.closest('.form__array');
  if (!fieldset) return;
  const rows = fieldset.querySelectorAll(':scope > .form__array-rows > .form__array-row');
  const anyExpanded = [...rows].some((row) => !row.classList.contains('form__array-row--collapsed'));
  rows.forEach(/** @param {HTMLElement} row */ (row) => {
    row.classList.toggle('form__array-row--collapsed', anyExpanded);
  });
  const icon = btn.querySelector('.material-symbols-outlined');
  if (icon) icon.textContent = anyExpanded ? 'unfold_more' : 'unfold_less';
}

/**
 * Move a row up.
 *
 * @param {HTMLButtonElement} btn
 */
function moveRowUp(btn) {
  const row = btn.closest('.form__array-row');
  if (!row || !row.previousElementSibling) return;
  row.parentElement.insertBefore(row, row.previousElementSibling);
  const fieldset = row.closest('.form__array');
  reindexRows(fieldset);
}

/**
 * Move a row down.
 *
 * @param {HTMLButtonElement} btn
 */
function moveRowDown(btn) {
  const row = btn.closest('.form__array-row');
  if (!row || !row.nextElementSibling) return;
  row.parentElement.insertBefore(row.nextElementSibling, row);
  const fieldset = row.closest('.form__array');
  reindexRows(fieldset);
}

/**
 * Duplicate a row. Respects max_rows.
 *
 * @param {HTMLButtonElement} btn
 */
function duplicateRow(btn) {
  const row = btn.closest('.form__array-row');
  if (!row) return;
  const fieldset = row.closest('.form__array');

  // Check max_rows before duplicating
  if (fieldset) {
    const addBtn = fieldset.querySelector('[data-max-rows]');
    if (addBtn) {
      const max = parseInt(addBtn.dataset.maxRows, 10);
      const container = fieldset.querySelector('.form__array-rows');
      if (container && container.children.length >= max) return;
    }
  }

  const clone = row.cloneNode(true);
  row.after(clone);
  // Re-initialize richtext editors in clone (they can't share state)
  clone.querySelectorAll('crap-richtext').forEach(
    /** @param {HTMLElement} el */ (el) => {
      if (el.connectedCallback) el.connectedCallback();
    }
  );
  reindexRows(fieldset);
  if (fieldset) {
    updateRowCount(fieldset);
    toggleEmptyState(fieldset);
    enforceMaxRows(fieldset);
    setupRowLabelWatcher(clone, fieldset);
  }
}

/**
 * Remove an array row. Re-indexes remaining rows.
 *
 * @param {HTMLButtonElement} btn
 */
function removeArrayRow(btn) {
  const row = btn.closest('.form__array-row');
  if (!row) return;

  const fieldset = row.closest('.form__array');

  row.remove();
  reindexRows(fieldset);

  if (fieldset) {
    updateRowCount(fieldset);
    toggleEmptyState(fieldset);
    enforceMaxRows(fieldset);
  }
}

/**
 * Add a new row to an array field repeater.
 *
 * @param {string} templateId
 */
function addArrayRow(templateId) {
  const template = document.getElementById(`array-template-${templateId}`);
  const container = document.getElementById(`array-rows-${templateId}`);
  if (!template || !container) return;

  const fieldset = container.closest('.form__array');

  // Check max_rows before adding
  if (fieldset) {
    const addBtn = fieldset.querySelector('[data-max-rows]');
    if (addBtn) {
      const max = parseInt(addBtn.dataset.maxRows, 10);
      if (container.children.length >= max) return;
    }
  }

  const nextIndex = container.children.length;
  const clone = /** @type {HTMLTemplateElement} */ (template).content.cloneNode(true);

  const html = /** @type {HTMLElement} */ (clone.firstElementChild);
  if (html) {
    replaceTemplateIndex(html, nextIndex);
  }

  container.appendChild(clone);
  if (html) {
    html.querySelectorAll('crap-richtext').forEach(
      /** @param {HTMLElement} el */ (el) => {
        if (el.connectedCallback) el.connectedCallback();
      }
    );
    if (fieldset) {
      setupRowLabelWatcher(html, fieldset);
    }
  }
  if (fieldset) {
    updateRowCount(fieldset);
    toggleEmptyState(fieldset);
    enforceMaxRows(fieldset);
  }
}

/**
 * Add a new block row to a blocks repeater.
 *
 * @param {string} templateId
 */
export function addBlockRow(templateId) {
  const typeSelect = /** @type {HTMLSelectElement} */ (
    document.getElementById(`block-type-${templateId}`)
  );
  if (!typeSelect) return;
  const blockType = typeSelect.value;
  const template = document.getElementById(`block-template-${templateId}-${blockType}`);
  const container = document.getElementById(`array-rows-${templateId}`);
  if (!template || !container) return;

  const fieldset = container.closest('.form__array');

  // Check max_rows before adding
  if (fieldset) {
    const addBtn = fieldset.querySelector('[data-max-rows]');
    if (addBtn) {
      const max = parseInt(addBtn.dataset.maxRows, 10);
      if (container.children.length >= max) return;
    }
  }

  const nextIndex = container.children.length;
  const clone = /** @type {HTMLTemplateElement} */ (template).content.cloneNode(true);

  const html = /** @type {HTMLElement} */ (clone.firstElementChild);
  if (html) {
    replaceTemplateIndex(html, nextIndex);
  }

  container.appendChild(clone);
  if (html) {
    html.querySelectorAll('crap-richtext').forEach(
      /** @param {HTMLElement} el */ (el) => {
        if (el.connectedCallback) el.connectedCallback();
      }
    );
    if (fieldset) {
      const blockLabelField = template.getAttribute('data-label-field');
      if (blockLabelField) {
        setupBlockRowLabelWatcher(html, blockLabelField);
      } else {
        setupRowLabelWatcher(html, fieldset);
      }
    }
  }
  if (fieldset) {
    updateRowCount(fieldset);
    toggleEmptyState(fieldset);
    enforceMaxRows(fieldset);
  }
}

/* ── Drag-and-drop sorting ─────────────────────────────────────── */

/** @type {HTMLElement|null} */
let draggedRow = null;

/**
 * Determine the element after which the dragged row should be inserted.
 *
 * @param {HTMLElement} container
 * @param {number} y
 * @returns {HTMLElement|null}
 */
function getDragAfterElement(container, y) {
  const rows = [...container.querySelectorAll('.form__array-row:not(.form__array-row--dragging)')];
  return rows.reduce((closest, child) => {
    const box = child.getBoundingClientRect();
    const offset = y - box.top - box.height / 2;
    if (offset < 0 && offset > closest.offset) {
      return { offset, element: child };
    }
    return closest;
  }, { offset: Number.NEGATIVE_INFINITY }).element || null;
}

registerDrag({
  start(el, e) {
    draggedRow = el.closest('.form__array-row');
    if (!draggedRow) return;
    draggedRow.classList.add('form__array-row--dragging');
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', '');
  },
  end() {
    if (draggedRow) {
      draggedRow.classList.remove('form__array-row--dragging');
      draggedRow = null;
    }
    document.querySelectorAll('.form__array-row--drag-over').forEach(
      (el) => el.classList.remove('form__array-row--drag-over')
    );
  },
  over(container, e) {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    if (!draggedRow) return;
    const afterElement = getDragAfterElement(container, e.clientY);
    container.querySelectorAll('.form__array-row--drag-over').forEach(
      (el) => el.classList.remove('form__array-row--drag-over')
    );
    if (afterElement) {
      afterElement.classList.add('form__array-row--drag-over');
    }
  },
  drop(container, e) {
    e.preventDefault();
    if (!draggedRow) return;
    const afterElement = getDragAfterElement(container, e.clientY);
    if (afterElement) {
      container.insertBefore(draggedRow, afterElement);
    } else {
      container.appendChild(draggedRow);
    }
    container.querySelectorAll('.form__array-row--drag-over').forEach(
      (el) => el.classList.remove('form__array-row--drag-over')
    );
    const fieldset = container.closest('.form__array');
    reindexRows(fieldset);
  },
});

/* ── Action registrations ────────────────────────────────────── */

registerAction('toggle-array-row', (el) => toggleArrayRow(el));
registerAction('toggle-all-rows', (el) => toggleAllRows(el));
registerAction('move-row-up', (el) => moveRowUp(el));
registerAction('move-row-down', (el) => moveRowDown(el));
registerAction('duplicate-row', (el) => duplicateRow(el));
registerAction('remove-array-row', (el) => removeArrayRow(el));
registerAction('add-array-row', (el) => addArrayRow(el.dataset.templateId));
registerAction('add-block-row', (el) => addBlockRow(el.dataset.templateId));
