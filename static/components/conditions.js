/**
 * Display conditions — `<crap-conditions>`.
 *
 * Toggles the visibility of fields whose `[data-condition]` (client-side
 * JSON) or `[data-condition-ref]` (server-side Lua function) evaluates
 * to false.
 *
 *  - **Client-side**: condition rows are JSON dictionaries combined with
 *    AND when wrapped in an array. Re-evaluated synchronously on every
 *    `input`/`change` in the form, against the form's *condition data*:
 *    the values decoded the way the server stores them (a checkbox as a
 *    boolean, a number as a number, a date as its UTC instant, text in its
 *    canonical form, an empty input as `null`, a group's values nested
 *    under the group) — the same view every server-side
 *    evaluation sees, so a condition decides the same way on the first
 *    render, after an error re-render and live.
 *  - **Server-side**: every interaction triggers a debounced
 *    `POST /admin/{collections|globals}/{slug}/evaluate-conditions`
 *    with the form's raw snapshot, which the server decodes into the same
 *    condition data — against the fields the form rendered for its viewer
 *    (the edited document's id and the editor locale travel along), so a
 *    field the viewer may not read is absent there as it is here. Results
 *    override field visibility per field name (the form name its wrapper
 *    carries, e.g. `seo__title`).
 *
 * @module conditions
 * @category form-field
 * @stability stable
 */

import { storedDate } from './_internal/stored-date.js';
import { csrfHeaders } from './_internal/util/csrf.js';
import { EV_CHANGE } from './events.js';

/**
 * @typedef {{ field?: string, equals?: any, not_equals?: any,
 *   in?: any[], not_in?: any[], is_truthy?: boolean, is_falsy?: boolean }} ConditionRow
 * @typedef {ConditionRow | ConditionRow[]} Condition
 *
 * @typedef {{ el: Element, type: string, fn: EventListener }} TrackedListener
 */

const SERVER_DEBOUNCE_MS = 300;

/**
 * The form events every re-evaluation follows: native edits, and the
 * `crap:change` custom inputs and array/blocks rows announce theirs with.
 */
const EDIT_EVENTS = ['input', 'change', EV_CHANGE];

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

/** Checkbox spellings the server reads as checked and unchecked (`core::parse_bool`). */
const CHECKED = ['1', 'true', 'yes', 'on'];
const UNCHECKED = ['0', 'false', 'no', 'off'];

/**
 * A number spelled as text, read as the server reads it (`core::parse_number`,
 * Rust's `f64` syntax around surrounding whitespace), or `null`.
 *
 * @param {string} raw
 * @returns {number | null}
 */
function parseNumber(raw) {
  const text = raw.trim();
  if (/^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/.test(text)) return Number(text);
  if (/^[+-]?(inf|infinity)$/i.test(text)) return text.startsWith('-') ? -Infinity : Infinity;
  if (/^[+-]?nan$/i.test(text)) return Number.NaN;
  return null;
}

/**
 * Whether a checkbox value reads as checked (`core::parse_truthy`): a checked
 * spelling, or any number but zero.
 *
 * @param {string} raw
 * @returns {boolean}
 */
function isChecked(raw) {
  const text = raw.trim().toLowerCase();
  if (CHECKED.includes(text)) return true;
  if (UNCHECKED.includes(text)) return false;
  const n = parseNumber(text);
  return n !== null && Math.abs(n) > 0;
}

/**
 * JS-style truthiness with the one departure the server shares
 * (`core::value_truthy`): an empty list is falsy.
 *
 * @param {unknown} val
 * @returns {boolean}
 */
function isTruthy(val) {
  if (Array.isArray(val)) return val.length > 0;
  return val !== null && val !== undefined && val !== '' && val !== false && val !== 0;
}

/**
 * Structural equality of two JSON values — what `serde_json::Value`'s `==`
 * compares on the server.
 *
 * @param {unknown} a
 * @param {unknown} b
 * @returns {boolean}
 */
function sameValue(a, b) {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== 'object' || typeof b !== 'object') return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;

  const left = /** @type {Record<string, unknown>} */ (a);
  const right = /** @type {Record<string, unknown>} */ (b);
  const keys = Object.keys(left);

  return (
    keys.length === Object.keys(right).length &&
    keys.every((k) => Object.hasOwn(right, k) && sameValue(left[k], right[k]))
  );
}

/**
 * The value a condition row's `field` names: a top-level field by name, a
 * field inside a group by its dotted (`seo.title`) or form-name
 * (`seo__title`) path. Missing is `null` — `ConditionRow::evaluate`'s
 * `field_value` on the server.
 *
 * @param {Record<string, unknown>} data
 * @param {string} field
 * @returns {unknown}
 */
function fieldValue(data, field) {
  /** @type {unknown} */
  let level = data;
  for (const key of field.split('.').flatMap((s) => s.split('__'))) {
    if (level === null || typeof level !== 'object' || Array.isArray(level)) return null;
    const obj = /** @type {Record<string, unknown>} */ (level);
    if (!Object.hasOwn(obj, key)) return null;
    level = obj[key];
  }
  return level ?? null;
}

/**
 * Evaluate one condition row (or AND-array of rows) against the condition
 * data — the same rules as `ConditionExpr::evaluate` on the server.
 *
 * @param {Condition} condition
 * @param {Record<string, unknown>} data
 * @returns {boolean}
 */
function evaluate(condition, data) {
  if (Array.isArray(condition)) {
    return condition.every((c) => evaluate(c, data));
  }
  if (!condition.field) return true;

  const value = fieldValue(data, condition.field);

  if ('equals' in condition) return sameValue(value, condition.equals);
  if ('not_equals' in condition) return !sameValue(value, condition.not_equals);
  if ('in' in condition) {
    return /** @type {any[]} */ (condition.in).some((v) => sameValue(value, v));
  }
  if ('not_in' in condition) {
    return !(/** @type {any[]} */ (condition.not_in).some((v) => sameValue(value, v)));
  }
  if (condition.is_truthy === true) return isTruthy(value);
  if (condition.is_falsy === true) return !isTruthy(value);
  return true;
}

/**
 * Split an input name into its data path: a group's `__` joins
 * (`seo__title` → `['seo', 'title']`) and a row's brackets
 * (`items[0][caption]` → `['items', 0, 'caption']`).
 *
 * @param {string} name
 * @returns {(string|number)[]}
 */
function namePath(name) {
  const [head, ...brackets] = name.split('[');
  /** @type {(string|number)[]} */
  const path = head.split('__');
  for (const part of brackets) {
    const key = part.replace(/\]$/, '');
    path.push(/^\d+$/.test(key) ? Number(key) : key);
  }
  return path;
}

/**
 * Store `value` at `path` in `target`, creating the objects and lists on the
 * way.
 *
 * @param {Record<string, any>} target
 * @param {(string|number)[]} path
 * @param {unknown} value
 */
function setPath(target, path, value) {
  /** @type {any} */
  let level = target;
  path.forEach((key, i) => {
    if (i === path.length - 1) {
      level[key] = value;
      return;
    }
    level[key] ??= typeof path[i + 1] === 'number' ? [] : {};
    level = level[key];
  });
}

/**
 * Drop the holes a list built by index can have, at every depth.
 *
 * @param {unknown} value
 * @returns {unknown}
 */
function compact(value) {
  if (Array.isArray(value)) return value.filter((v) => v !== undefined).map(compact);
  if (value === null || typeof value !== 'object') return value;
  const obj = /** @type {Record<string, unknown>} */ (value);
  for (const key of Object.keys(obj)) obj[key] = compact(obj[key]);
  return obj;
}

/**
 * A number spelled as text as a number; anything else — and a number the
 * server cannot store (infinite, not a number) — as sent.
 *
 * @param {string} raw
 * @returns {string|number}
 */
function toNumber(raw) {
  const n = parseNumber(raw);
  return n !== null && Number.isFinite(n) ? n : raw;
}

/**
 * The stored form of a text value (`core::canonical_text`): an email trimmed,
 * lowercased and NFC-composed, other text NFC-composed.
 *
 * @param {string} kind
 * @param {string} raw
 * @returns {string}
 */
function canonicalText(kind, raw) {
  if (kind === 'email') return raw.trim().toLowerCase().normalize('NFC');
  return raw.normalize('NFC');
}

/**
 * The items of one submitted list value: a JSON array (what the tag widget
 * and a read-only list write) or a single item.
 *
 * @param {string} raw
 * @returns {string[]}
 */
function listItems(raw) {
  if (raw.trim() === '') return [];
  try {
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) return parsed.filter((v) => v !== null).map(String);
  } catch {
    // Not JSON — one item.
  }
  return [raw];
}

/**
 * JSON text as its value; anything that does not parse as sent.
 *
 * @param {string} raw
 * @returns {unknown}
 */
function parseJson(raw) {
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

/**
 * Whether the field behind `wrapper` stores its text as a JSON value: a JSON
 * field, or rich text in the JSON format.
 *
 * @param {HTMLElement|undefined} wrapper
 * @returns {boolean}
 */
function storesJson(wrapper) {
  const kind = wrapper?.dataset.kind;
  if (kind === 'json') return true;
  return kind === 'richtext' && wrapper?.querySelector('[data-format="json"]') != null;
}

/** The field types whose text is stored in a canonical form. */
const TEXT_KINDS = ['text', 'textarea', 'email'];

/**
 * The items of a `has_many` value as the server stores them: a number list
 * keeps only the items that are numbers it can store, every other list its
 * items as sent.
 *
 * @param {string} kind
 * @param {string[]} values
 * @returns {unknown[]}
 */
function decodeList(kind, values) {
  const items = values.flatMap(listItems);
  if (kind !== 'number') return items;
  return items.map(parseNumber).filter((n) => n !== null && Number.isFinite(n));
}

/**
 * Decode one submitted value of a single-valued field the way the write
 * stores it.
 *
 * @param {HTMLElement} wrapper
 * @param {string} raw
 * @param {string} [zone] The timezone chosen beside a date that stores one.
 * @returns {unknown}
 */
function decodeScalar(wrapper, raw, zone) {
  const kind = wrapper.dataset.kind ?? '';
  if (raw === '') return null;
  if (storesJson(wrapper)) return parseJson(raw);
  if (kind === 'number') return toNumber(raw);
  if (kind === 'date') return storedDate(raw, zone);
  if (TEXT_KINDS.includes(kind)) return canonicalText(kind, raw);
  return raw;
}

/**
 * Decode the values the form submits under one name the way the write path
 * stores them: a checkbox as a boolean, a number as a number, a date as its
 * UTC instant, text in its canonical form, JSON text as its value, an empty
 * input as `null`, a `has_many` field as a list. A name that is no field's
 * (a date's timezone beside it, say) keeps its value as sent.
 *
 * @param {HTMLElement|undefined} wrapper The field's wrapper, if the name is a field's.
 * @param {string[]} values
 * @param {string} [zone] The timezone chosen beside a date that stores one.
 * @returns {unknown}
 */
function decodeValue(wrapper, values, zone) {
  if (!wrapper) return values.length > 1 ? values : (values[0] ?? '');

  const kind = wrapper.dataset.kind ?? '';
  if (kind === 'checkbox') return values.some(isChecked);
  if (wrapper.hasAttribute('data-has-many')) return decodeList(kind, values);

  if (values.length > 1) return values.map((raw) => decodeScalar(wrapper, raw, zone));
  return decodeScalar(wrapper, values[0] ?? '', zone);
}

/**
 * The values every control of `form` submits, by name — an unchecked box or
 * radio, and a select with nothing picked, under its name with no value.
 * Meta inputs (`_csrf`, `_action`, …), files, buttons and disabled controls
 * submit nothing and are skipped.
 *
 * @param {HTMLFormElement} form
 * @returns {Map<string, string[]>}
 */
function submittedValues(form) {
  /** @type {Map<string, string[]>} */
  const byName = new Map();
  for (const el of /** @type {HTMLCollectionOf<HTMLInputElement>} */ (form.elements)) {
    const { name } = el;
    if (!name || name.startsWith('_') || el.disabled) continue;
    if (['file', 'submit', 'button', 'reset', 'image'].includes(el.type)) continue;

    const values = byName.get(name) ?? [];
    byName.set(name, values);

    if ((el.type === 'checkbox' || el.type === 'radio') && !el.checked) continue;
    if (el instanceof HTMLSelectElement) {
      for (const option of el.selectedOptions) values.push(option.value);
      continue;
    }
    values.push(el.value);
  }
  return byName;
}

/**
 * The form's condition data: its values decoded like the server decodes a
 * submission (the `condition_data` view every server-side evaluation sees),
 * group values nested under their group and row values under their list.
 *
 * @param {HTMLFormElement} form
 * @returns {Record<string, unknown>}
 */
function conditionData(form) {
  /** @type {Map<string, HTMLElement>} */
  const wrappers = new Map();
  for (const el of /** @type {NodeListOf<HTMLElement>} */ (
    form.querySelectorAll('[data-field-name][data-kind]')
  )) {
    wrappers.set(el.dataset.fieldName ?? '', el);
  }

  const submitted = submittedValues(form);

  /** @type {Record<string, unknown>} */
  const data = {};
  for (const [name, values] of submitted) {
    const wrapper = wrappers.get(name);
    const zone = wrapper?.hasAttribute('data-timezone') ? submitted.get(`${name}_tz`)?.[0] : '';
    setPath(data, namePath(name), decodeValue(wrapper, values, zone));
  }
  return /** @type {Record<string, unknown>} */ (compact(data));
}

/**
 * The raw snapshot the evaluate endpoint takes — each input name to its
 * value, a repeated name as a list: the payload the validate endpoint takes
 * too. The server decodes it exactly as a submission of the form.
 *
 * @param {HTMLFormElement} form
 * @returns {Record<string, string | string[]>}
 */
function formSnapshot(form) {
  /** @type {Record<string, string | string[]>} */
  const snapshot = {};
  for (const [key, val] of new FormData(form).entries()) {
    if (key.startsWith('_') || val instanceof File) continue;
    const cur = snapshot[key];
    if (cur === undefined) snapshot[key] = val;
    else snapshot[key] = Array.isArray(cur) ? [...cur, val] : [cur, val];
  }
  return snapshot;
}

/**
 * The editor locale the form was rendered in — its `_locale` input, which
 * {@link formSnapshot} leaves out with every other `_`-prefixed control.
 *
 * @param {HTMLFormElement} form
 * @returns {string|null}
 */
function formLocale(form) {
  const input = form.elements.namedItem('_locale');
  return input instanceof HTMLInputElement && input.value ? input.value : null;
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
      for (const type of EDIT_EVENTS) form.removeEventListener(type, this._debouncedServer);
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
   * Wire client-side conditions. Every condition is re-evaluated against the
   * form's condition data on any `input`/`change` in the form (a radio
   * group's click, a nested group's field, a row add included) and on
   * `crap:change`, which the custom inputs (relationship, upload, tags) and
   * the array/blocks rows announce edits with.
   *
   * @param {HTMLFormElement} form
   * @param {NodeListOf<Element>} clientFields
   */
  _setupClient(form, clientFields) {
    const run = () => {
      const data = conditionData(form);
      for (const el of clientFields) {
        const cond = parseCondition(el);
        if (!cond) continue;
        el.classList.toggle('form__field--hidden', !evaluate(cond, data));
      }
    };

    for (const type of EDIT_EVENTS) {
      form.addEventListener(type, run);
      this._clientListeners.push({ el: form, type, fn: run });
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
    const operation = this.getAttribute('operation') || 'update';
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

      const headers = csrfHeaders({ 'Content-Type': 'application/json' });

      try {
        const res = await fetch(url, {
          method: 'POST',
          headers,
          body: JSON.stringify({
            form_data: formSnapshot(form),
            conditions: refs,
            operation,
            document_id: form.dataset.documentId || null,
            locale: formLocale(form),
          }),
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

    for (const type of EDIT_EVENTS) form.addEventListener(type, this._debouncedServer);
  }
}

customElements.define('crap-conditions', CrapConditions);
