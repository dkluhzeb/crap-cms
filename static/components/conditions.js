/**
 * Display conditions — `<crap-conditions>`.
 *
 * Toggles the visibility of fields whose `[data-condition]` (client-side
 * JSON) or `[data-condition-ref]` (server-side Lua function) evaluates
 * to false.
 *
 *  - **Client-side**: condition rows are JSON dictionaries combined with
 *    AND when wrapped in an array. Re-evaluated synchronously on every
 *    `input`/`change` of any watched field.
 *  - **Server-side**: every interaction triggers a debounced
 *    `POST /admin/{collections|globals}/{slug}/evaluate-conditions`
 *    with the current form data. Results override field visibility per
 *    field name.
 *
 * @module conditions
 */

import { readCsrfCookie } from './util/cookies.js';

/**
 * @typedef {{ field?: string, equals?: any, not_equals?: any,
 *   in?: any[], not_in?: any[], is_truthy?: boolean, is_falsy?: boolean }} ConditionRow
 * @typedef {ConditionRow | ConditionRow[]} Condition
 *
 * @typedef {{ el: Element, type: string, fn: EventListener }} TrackedListener
 */

const SERVER_DEBOUNCE_MS = 300;

/**
 * Parse a `[data-condition]` JSON blob, returning `null` if the blob is
 * missing or malformed.
 *
 * @param {Element} el
 * @returns {Condition | null}
 */
function parseCondition(el) {
  const raw = /** @type {HTMLElement} */ (el).dataset.condition;
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

/**
 * Drop "object-coerces-to-truthy" oddities that don't make sense for
 * field-presence checks (empty arrays, the literal string `''`, etc.)
 * while preserving the legacy semantics for other types.
 *
 * @param {unknown} val
 * @returns {boolean}
 */
function isTruthy(val) {
  if (Array.isArray(val)) return val.length > 0;
  return val !== null && val !== undefined && val !== '' && val !== false && val !== 0;
}

/**
 * Evaluate one condition row (or AND-array of rows) against form data.
 *
 * @param {Condition} condition
 * @param {Record<string, unknown>} formData
 * @returns {boolean}
 */
function evaluate(condition, formData) {
  if (Array.isArray(condition)) {
    return condition.every((c) => evaluate(c, formData));
  }
  const fieldVal = condition.field !== undefined ? (formData[condition.field] ?? '') : '';

  if ('equals' in condition) return fieldVal === condition.equals;
  if ('not_equals' in condition) return fieldVal !== condition.not_equals;
  if ('in' in condition) return /** @type {any[]} */ (condition.in).includes(fieldVal);
  if ('not_in' in condition) return !(/** @type {any[]} */ (condition.not_in).includes(fieldVal));
  if (condition.is_truthy) return isTruthy(fieldVal);
  if (condition.is_falsy) return !isTruthy(fieldVal);
  return true;
}

/**
 * Walk a condition tree collecting every `field` reference into `out`.
 *
 * @param {Condition} condition
 * @param {Set<string>} out
 */
function collectFields(condition, out) {
  if (Array.isArray(condition)) {
    for (const c of condition) collectFields(c, out);
    return;
  }
  if (condition?.field) out.add(condition.field);
}

/**
 * Snapshot a form into a plain object. Internal fields (`_csrf`,
 * `_action`, …) are skipped. Repeated keys produce arrays; checkboxes
 * absent from `FormData` are emitted as `''` so unchecked state is
 * observable in conditions.
 *
 * @param {HTMLFormElement} form
 * @returns {Record<string, string | string[]>}
 */
function collectFormData(form) {
  /** @type {Record<string, string | string[]>} */
  const data = {};
  for (const [key, val] of new FormData(form).entries()) {
    if (key.startsWith('_')) continue;
    const str = /** @type {string} */ (val);
    if (key in data) {
      const cur = data[key];
      data[key] = Array.isArray(cur) ? [...cur, str] : [cur, str];
    } else {
      data[key] = str;
    }
  }
  for (const cb of /** @type {NodeListOf<HTMLInputElement>} */ (
    form.querySelectorAll('input[type="checkbox"]')
  )) {
    if (cb.name.startsWith('_') || cb.name in data) continue;
    data[cb.name] = cb.checked ? 'on' : '';
  }
  return data;
}

class CrapConditions extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._initialized = false;
    /** @type {ReturnType<typeof setTimeout>|null} */
    this._serverTimer = null;
    /** @type {AbortController|null} */
    this._serverAbort = null;
    /** @type {TrackedListener[]} */
    this._clientListeners = [];
    /** @type {EventListener|null} */
    this._debouncedServer = null;
  }

  connectedCallback() {
    if (this._initialized) return;
    this._initialized = true;

    const form = this._getForm();
    if (!form) return;

    const clientFields = this.querySelectorAll('[data-condition]');
    const serverFields = this.querySelectorAll('[data-condition-ref]');
    if (clientFields.length === 0 && serverFields.length === 0) return;

    if (clientFields.length > 0) this._setupClient(form, clientFields);
    if (serverFields.length > 0) this._setupServer(form, serverFields);
  }

  disconnectedCallback() {
    if (this._serverTimer) clearTimeout(this._serverTimer);
    if (this._serverAbort) this._serverAbort.abort();

    const form = this._debouncedServer ? this._getForm() : null;
    if (form && this._debouncedServer) {
      form.removeEventListener('input', this._debouncedServer);
      form.removeEventListener('change', this._debouncedServer);
    }

    for (const { el, type, fn } of this._clientListeners) {
      el.removeEventListener(type, fn);
    }
    this._clientListeners = [];
    this._debouncedServer = null;
    this._initialized = false;
  }

  /** @returns {HTMLFormElement|null} */
  _getForm() {
    return /** @type {HTMLFormElement|null} */ (this.querySelector('form') || this.closest('form'));
  }

  /**
   * Wire client-side conditions. Each row is re-evaluated on `input`/
   * `change` of any field its condition references.
   *
   * @param {HTMLFormElement} form
   * @param {NodeListOf<Element>} clientFields
   */
  _setupClient(form, clientFields) {
    /** @type {Set<string>} */
    const watched = new Set();
    for (const el of clientFields) {
      const cond = parseCondition(el);
      if (cond) collectFields(cond, watched);
    }

    const run = () => {
      const data = collectFormData(form);
      for (const el of clientFields) {
        const cond = parseCondition(el);
        if (!cond) continue;
        el.classList.toggle('form__field--hidden', !evaluate(cond, data));
      }
    };

    for (const fieldName of watched) {
      const input = form.querySelector(`[name="${CSS.escape(fieldName)}"]`);
      if (!input) continue;
      for (const type of /** @type {const} */ (['input', 'change'])) {
        input.addEventListener(type, run);
        this._clientListeners.push({ el: input, type, fn: run });
      }
    }
  }

  /**
   * Wire server-side conditions. The form posts the current data to the
   * evaluate-conditions endpoint with a {@link SERVER_DEBOUNCE_MS}
   * debounce; each new evaluation aborts any in-flight request so a
   * stale response can't overwrite a newer result.
   *
   * @param {HTMLFormElement} form
   * @param {NodeListOf<Element>} serverFields
   */
  _setupServer(form, serverFields) {
    const slug = this.getAttribute('collection') || form.dataset.collectionSlug || '';
    const isGlobal = this.getAttribute('type') === 'global';
    const url = `${isGlobal ? '/admin/globals/' : '/admin/collections/'}${slug}/evaluate-conditions`;

    const run = async () => {
      /** @type {Record<string, string>} */
      const refs = {};
      for (const el of /** @type {NodeListOf<HTMLElement>} */ (serverFields)) {
        const name = el.dataset.fieldName;
        const ref = el.dataset.conditionRef;
        if (name && ref) refs[name] = ref;
      }

      if (this._serverAbort) this._serverAbort.abort();
      this._serverAbort = new AbortController();

      const csrf = readCsrfCookie();
      /** @type {Record<string, string>} */
      const headers = { 'Content-Type': 'application/json' };
      if (csrf) headers['X-CSRF-Token'] = csrf;

      try {
        const res = await fetch(url, {
          method: 'POST',
          headers,
          body: JSON.stringify({ form_data: collectFormData(form), conditions: refs }),
          signal: this._serverAbort.signal,
        });
        /** @type {Record<string, boolean>} */
        const result = await res.json();
        for (const [fieldName, visible] of Object.entries(result)) {
          const el = this.querySelector(
            `[data-field-name="${CSS.escape(fieldName)}"][data-condition-ref]`,
          );
          if (el) el.classList.toggle('form__field--hidden', !visible);
        }
      } catch {
        // Silent fail — keep current visibility on network/abort errors.
      }
    };

    this._debouncedServer = () => {
      if (this._serverTimer) clearTimeout(this._serverTimer);
      this._serverTimer = setTimeout(run, SERVER_DEBOUNCE_MS);
    };

    form.addEventListener('input', this._debouncedServer);
    form.addEventListener('change', this._debouncedServer);
  }
}

customElements.define('crap-conditions', CrapConditions);
