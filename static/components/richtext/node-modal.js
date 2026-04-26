/**
 * Custom-node edit modal for `<crap-richtext>`.
 *
 * Schema-driven form generated from a `CustomNodeDef`. On submit,
 * tentatively applies the new attrs (so the parent form's validate
 * endpoint serializes correctly), validates, and either commits or
 * reverts + surfaces per-attr errors.
 *
 * @module richtext/node-modal
 */

import { h } from '../h.js';
import { t } from '../i18n.js';

/**
 * @typedef {{ value: string, label: string }} NodeAttrOption
 *
 * @typedef {{
 *   name: string,
 *   type: string,
 *   label: string,
 *   required?: boolean,
 *   readonly?: boolean,
 *   hidden?: boolean,
 *   default?: any,
 *   placeholder?: string,
 *   description?: string,
 *   width?: string,
 *   min?: any,
 *   max?: any,
 *   step?: any,
 *   min_length?: any,
 *   max_length?: any,
 *   min_date?: string,
 *   max_date?: string,
 *   language?: string,
 *   rows?: number,
 *   picker_appearance?: string,
 *   options?: NodeAttrOption[],
 * }} NodeAttrSpec
 *
 * @typedef {{ name: string, label: string, attrs?: NodeAttrSpec[] }} CustomNodeDef
 *
 * @typedef {{
 *   shadowRoot: ShadowRoot|null,
 *   _view: any,
 *   querySelector: (sel: string) => Element|null,
 *   closest: (sel: string) => Element|null,
 *   _applyNodeAttrs: (pos: number, attrs: any, expectedType?: string) => void,
 *   _getNodeIndex: (nodeType: string, pos: number) => number,
 * }} HostAPI
 */

/**
 * Open the node-edit modal inside the host's shadow root. Replaces any
 * existing `.crap-node-modal` first.
 *
 * @param {HostAPI} host
 * @param {CustomNodeDef} nodeDef
 * @param {Record<string, any>} attrs
 * @param {number} pos
 */
export function openNodeEditModal(host, nodeDef, attrs, pos) {
  if (!host.shadowRoot) return;
  host.shadowRoot.querySelector('.crap-node-modal')?.remove();

  const modal = buildNodeModal(nodeDef, attrs);
  host.shadowRoot.appendChild(modal);
  applyFieldWidths(modal);
  modal.showModal();

  /** @type {HTMLElement|null} */
  const firstInput = modal.querySelector('input, textarea, select');
  firstInput?.focus();

  const close = () => {
    modal.close();
    modal.remove();
  };
  modal.addEventListener('cancel', (e) => {
    e.preventDefault();
    close();
  });
  modal.querySelector('.crap-node-modal__btn--cancel')?.addEventListener('click', close);
  modal
    .querySelector('.crap-node-modal__btn--ok')
    ?.addEventListener('click', () => submitNodeEdit(host, modal, nodeDef, attrs, pos, close));
}

/* ── Modal construction ─────────────────────────────────────────── */

/**
 * @param {CustomNodeDef} nodeDef
 * @param {Record<string, any>} attrs
 */
function buildNodeModal(nodeDef, attrs) {
  const fields = (nodeDef.attrs || [])
    .filter((a) => !a.hidden)
    .map((a) => buildNodeField(nodeDef, attrs, a));

  return h(
    'dialog',
    {
      class: 'crap-node-modal',
      'aria-labelledby': 'crap-node-modal-heading',
    },
    h(
      'div',
      { class: 'crap-node-modal__dialog' },
      h('div', {
        class: 'crap-node-modal__header',
        id: 'crap-node-modal-heading',
        text: nodeDef.label,
      }),
      h('div', { class: 'crap-node-modal__body' }, ...fields),
      h(
        'div',
        { class: 'crap-node-modal__footer' },
        h('button', {
          type: 'button',
          class: ['crap-node-modal__btn', 'crap-node-modal__btn--cancel'],
          text: t('cancel'),
        }),
        h('button', {
          type: 'button',
          class: ['crap-node-modal__btn', 'crap-node-modal__btn--ok'],
          text: t('ok'),
        }),
      ),
    ),
  );
}

/**
 * Apply per-field widths programmatically. We can't use inline `style="…"`
 * (CSP `style-src 'self'` blocks it). The CSS already styles the standard
 * widths via `[data-field-width="50"]` etc.; this is the override path
 * for unknown values.
 *
 * @param {HTMLDialogElement} modal
 */
function applyFieldWidths(modal) {
  for (const field of /** @type {NodeListOf<HTMLElement>} */ (
    modal.querySelectorAll('[data-field-width]')
  )) {
    const w = field.dataset.fieldWidth;
    if (w) field.style.width = w;
  }
}

/* ── Submit / validation flow ───────────────────────────────────── */

/**
 * @param {HostAPI} host
 * @param {HTMLDialogElement} modal
 * @param {CustomNodeDef} nodeDef
 * @param {Record<string, any>} attrs
 * @param {number} pos
 * @param {() => void} close
 */
async function submitNodeEdit(host, modal, nodeDef, attrs, pos, close) {
  const newAttrs = collectNodeAttrs(modal, nodeDef, attrs);

  /** @type {any} */
  const validateForm = host.closest('crap-validate-form');
  if (!validateForm || typeof validateForm.getValidationErrors !== 'function') {
    // No validation available — apply and close.
    host._applyNodeAttrs(pos, newAttrs, nodeDef.name);
    close();
    host._view?.focus();
    return;
  }

  // Apply tentatively so the textarea serializes correctly for validation.
  host._applyNodeAttrs(pos, newAttrs, nodeDef.name);

  /** @type {HTMLButtonElement|null} */
  const okBtn = modal.querySelector('.crap-node-modal__btn--ok');
  if (okBtn) {
    okBtn.disabled = true;
    okBtn.textContent = t('validating');
  }
  clearDialogErrors(modal);

  const errors = await validateForm.getValidationErrors();
  if (errors === null) {
    // Network error — keep new attrs, close gracefully.
    close();
    host._view?.focus();
    return;
  }

  const attrErrors = filterErrorsForNode(host, errors, nodeDef, pos);
  if (Object.keys(attrErrors).length === 0) {
    close();
    host._view?.focus();
    return;
  }

  // Validation failed — revert to original attrs, surface field errors.
  host._applyNodeAttrs(pos, attrs, nodeDef.name);
  showDialogErrors(modal, attrErrors);
  if (okBtn) {
    okBtn.disabled = false;
    okBtn.textContent = t('ok');
  }
}

/**
 * @param {HTMLDialogElement} modal
 * @param {CustomNodeDef} nodeDef
 * @param {Record<string, any>} attrs
 */
function collectNodeAttrs(modal, nodeDef, attrs) {
  /** @type {Record<string, any>} */
  const newAttrs = {};
  for (const a of nodeDef.attrs || []) {
    if (a.hidden) {
      newAttrs[a.name] = attrs[a.name] ?? a.default ?? '';
      continue;
    }
    const el = modal.querySelector(`[data-attr="${a.name}"]`);
    if (!el) continue;
    if (a.type === 'checkbox') {
      newAttrs[a.name] = /** @type {HTMLInputElement} */ (el).checked;
    } else if (a.type === 'radio') {
      const checked = /** @type {HTMLInputElement|null} */ (
        el.querySelector('input[type="radio"]:checked')
      );
      newAttrs[a.name] = checked?.value || '';
    } else {
      newAttrs[a.name] = /** @type {HTMLInputElement} */ (el).value;
    }
  }
  return newAttrs;
}

/**
 * Narrow the form-wide error map to the per-attr errors that belong to
 * this node instance. Form errors are keyed `fieldName[type#index].attr`.
 *
 * @param {HostAPI} host
 * @param {Record<string, string>} errors
 * @param {CustomNodeDef} nodeDef
 * @param {number} pos
 * @returns {Record<string, string>}
 */
function filterErrorsForNode(host, errors, nodeDef, pos) {
  const textarea = host.querySelector('textarea');
  const fieldName = textarea ? /** @type {HTMLTextAreaElement} */ (textarea).name : '';
  const nodeIndex = host._getNodeIndex(nodeDef.name, pos);
  const prefix = `${fieldName}[${nodeDef.name}#${nodeIndex}].`;

  /** @type {Record<string, string>} */
  const out = {};
  for (const [key, message] of Object.entries(errors)) {
    if (key.startsWith(prefix)) out[key.slice(prefix.length)] = message;
  }
  return out;
}

/* ── Per-attr field rendering ───────────────────────────────────── */

/**
 * Build a single field element for the modal. Every value flows through
 * `setAttribute` / `textContent` (via `h()`), so HTML injection is
 * unwriteable regardless of `attrs[a.name]` content.
 *
 * @param {CustomNodeDef} nodeDef
 * @param {Record<string, any>} attrs
 * @param {NodeAttrSpec} a
 * @returns {HTMLDivElement}
 */
function buildNodeField(nodeDef, attrs, a) {
  const val = attrs[a.name] ?? a.default ?? '';
  const inputId = `crap-node-${nodeDef.name}-${a.name}`;
  const ro = !!a.readonly;
  const req = !!a.required;
  const ph = a.placeholder || undefined;

  const input = buildNodeFieldInput(a, val, inputId, ro, req, ph);

  const wrapper = h('div', {
    class: 'crap-node-modal__field',
    dataset: a.width ? { fieldWidth: a.width } : undefined,
  });

  if (a.type === 'checkbox') {
    wrapper.append(input);
  } else {
    const langSuffix = a.language ? ` (${a.language})` : '';
    wrapper.append(
      h('label', {
        class: 'crap-node-modal__label',
        for: inputId,
        text: `${a.label}${langSuffix}${a.required ? ' *' : ''}`,
      }),
      input,
    );
  }

  if (a.description) {
    wrapper.append(h('p', { class: 'crap-node-modal__help', text: a.description }));
  }
  return wrapper;
}

/**
 * Build the bare input/select/textarea for one custom-node attribute.
 *
 * @param {NodeAttrSpec} a
 * @param {any} val
 * @param {string} inputId
 * @param {boolean} ro
 * @param {boolean} req
 * @param {string|undefined} ph
 * @returns {HTMLElement}
 */
function buildNodeFieldInput(a, val, inputId, ro, req, ph) {
  const common = {
    class: 'crap-node-modal__input',
    id: inputId,
    dataset: { attr: a.name },
    placeholder: ph,
    required: req,
    readonly: ro,
    disabled: ro,
  };

  switch (a.type) {
    case 'textarea':
      return h(
        'textarea',
        {
          ...common,
          rows: a.rows || 3,
          minlength: a.min_length,
          maxlength: a.max_length,
        },
        String(val),
      );
    case 'checkbox':
      return h(
        'label',
        { class: 'crap-node-modal__checkbox' },
        h('input', {
          type: 'checkbox',
          id: inputId,
          dataset: { attr: a.name },
          checked: !!val,
          readonly: ro,
          disabled: ro,
        }),
        ` ${a.label}`,
      );
    case 'select':
      return h(
        'select',
        common,
        ...(a.options || []).map((o) =>
          h('option', { value: o.value, selected: o.value === val, text: o.label }),
        ),
      );
    case 'radio':
      return h(
        'div',
        { class: 'crap-node-modal__radio-group', dataset: { attr: a.name } },
        ...(a.options || []).map((o, i) =>
          h(
            'label',
            { class: 'crap-node-modal__radio' },
            h('input', {
              type: 'radio',
              id: `${inputId}-${i}`,
              name: `node-attr-${a.name}`,
              value: o.value,
              checked: o.value === val,
              readonly: ro,
              disabled: ro,
            }),
            ` ${o.label}`,
          ),
        ),
      );
    case 'number':
      return h('input', {
        ...common,
        type: 'number',
        value: val,
        min: a.min,
        max: a.max,
        step: a.step,
      });
    case 'email':
      return h('input', {
        ...common,
        type: 'email',
        value: val,
        minlength: a.min_length,
        maxlength: a.max_length,
      });
    case 'date':
      return h('input', {
        ...common,
        type: dateInputType(a.picker_appearance),
        value: val,
        min: a.min_date,
        max: a.max_date,
      });
    case 'code':
    case 'json':
      return h(
        'textarea',
        {
          ...common,
          class: ['crap-node-modal__input', 'crap-node-modal__input--mono'],
          rows: a.rows || 4,
          minlength: a.min_length,
          maxlength: a.max_length,
        },
        String(val),
      );
    default:
      return h('input', {
        ...common,
        type: 'text',
        value: val,
        minlength: a.min_length,
        maxlength: a.max_length,
      });
  }
}

/** @param {string|undefined} appearance */
function dateInputType(appearance) {
  switch (appearance) {
    case 'dayAndTime':
      return 'datetime-local';
    case 'timeOnly':
      return 'time';
    case 'monthOnly':
      return 'month';
    default:
      return 'date';
  }
}

/* ── Per-field error UI ─────────────────────────────────────────── */

/**
 * @param {HTMLElement} modal
 * @param {Record<string, string>} attrErrors
 */
export function showDialogErrors(modal, attrErrors) {
  clearDialogErrors(modal);
  for (const [attrName, message] of Object.entries(attrErrors)) {
    const input = modal.querySelector(`[data-attr="${attrName}"]`);
    if (!input) continue;
    input.classList.add('crap-node-modal__input--error');
    const field = input.closest('.crap-node-modal__field');
    if (field) {
      field.appendChild(h('p', { class: 'crap-node-modal__error', text: message }));
    }
  }
}

/** @param {HTMLElement} modal */
export function clearDialogErrors(modal) {
  for (const el of modal.querySelectorAll('.crap-node-modal__error')) el.remove();
  for (const el of modal.querySelectorAll('.crap-node-modal__input--error')) {
    el.classList.remove('crap-node-modal__input--error');
  }
}
