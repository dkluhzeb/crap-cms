/**
 * Pre-submit validation for upload forms — `<crap-validate-form>`.
 *
 * Wraps a form that may contain file inputs. On submit, intercepts the
 * HTMX request, posts the non-file data to a JSON endpoint for
 * validation, and only lets the real submission proceed (file and
 * all) if validation passes. This avoids the lost-file-on-error UX
 * problem (browsers can't re-populate file inputs after a failed
 * round-trip).
 *
 * Side effects on validation failure:
 *  - `<p class="form__error" data-validate-error>` injected next to
 *    the offending field (and the row marker on its array container).
 *  - Richtext fields receive their per-node errors via
 *    `crap-richtext.markNodeErrors()` so the editor can highlight the
 *    bad atoms in place.
 *  - A red toast summarises that validation failed.
 *
 * @example
 * <crap-validate-form validate-url="/admin/collections/media/validate">
 *   <form id="edit-form" ...>...</form>
 * </crap-validate-form>
 *
 * @module validate-form
 */

import { h } from './h.js';
import { t } from './i18n.js';

/** @returns {string} */
function readCsrfCookie() {
  const m = document.cookie.match(/(?:^|;\s*)crap_csrf=([^;]*)/);
  if (!m) return '';
  try { return decodeURIComponent(m[1]); } catch { return m[1]; }
}

/** @param {string} message @param {'error'|'success'|'info'} type */
function toast(message, type) {
  document.dispatchEvent(new CustomEvent('crap:toast', { detail: { message, type } }));
}

/**
 * @typedef {{
 *   data: Record<string, any>,
 *   draft: boolean,
 *   locale: string|null,
 * }} CollectedFormData
 */

class CrapValidateForm extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {boolean} */
    this._validated = false;
    /** @type {boolean} */
    this._validating = false;
    /** @type {string} */
    this._validateUrl = '';
    /** @type {((e: Event) => void)|null} */
    this._onBeforeRequest = null;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    this._validateUrl = this.getAttribute('validate-url') || '';

    this._onBeforeRequest = (e) => this._interceptSubmit(e);
    document.body.addEventListener('htmx:beforeRequest', this._onBeforeRequest);
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    if (this._onBeforeRequest) {
      document.body.removeEventListener('htmx:beforeRequest', this._onBeforeRequest);
      this._onBeforeRequest = null;
    }
  }

  /* ── Submit interception ────────────────────────────────────── */

  /** @param {Event} e */
  _interceptSubmit(e) {
    const evt = /** @type {CustomEvent} */ (e);
    /** @type {HTMLFormElement|null} */
    const form = this.querySelector('#edit-form');
    if (!form || evt.detail.elt !== form) return;

    // Already validated → let the real request through.
    if (this._validated) {
      this._validated = false;
      return;
    }

    evt.preventDefault();
    this._runValidation(form);
  }

  /**
   * POST the form data to the validate endpoint and act on the result.
   * Re-triggers the original submit if valid; renders inline errors
   * if invalid; toasts a server-error message on network failure.
   *
   * @param {HTMLFormElement} form
   */
  async _runValidation(form) {
    if (this._validating) return;
    this._validating = true;
    try {
      if (!this._validateUrl) {
        this._validated = true;
        this._retrigger(form);
        return;
      }

      const errors = await this.getValidationErrors();
      if (errors === null) {
        toast(t('validation.server_error'), 'error');
        return;
      }
      if (Object.keys(errors).length === 0) {
        this._clearErrors();
        this._validated = true;
        this._retrigger(form);
        return;
      }
      this._showErrors(errors);
    } finally {
      this._validating = false;
    }
  }

  /**
   * Re-fire the form's submit. With HTMX present we use its trigger
   * helper; otherwise the native `requestSubmit`.
   *
   * @param {HTMLFormElement} form
   */
  _retrigger(form) {
    if (typeof htmx !== 'undefined') {
      htmx.trigger(form, 'submit');
      return;
    }
    form.requestSubmit();
  }

  /* ── Public validation API ──────────────────────────────────── */

  /**
   * Run validation against the JSON endpoint.
   *  - `{}` ⇒ valid
   *  - `Record<string, string>` ⇒ field → message map
   *  - `null` ⇒ network error (caller decides what to do)
   *
   * @returns {Promise<Record<string, string>|null>}
   */
  async getValidationErrors() {
    if (!this._validateUrl) return {};
    /** @type {HTMLFormElement|null} */
    const form = this.querySelector('#edit-form');
    if (!form) return {};

    const csrf = readCsrfCookie();
    /** @type {Record<string, string>} */
    const headers = { 'Content-Type': 'application/json' };
    if (csrf) headers['X-CSRF-Token'] = csrf;

    try {
      const res = await fetch(this._validateUrl, {
        method: 'POST',
        headers,
        body: JSON.stringify(this._collectFormData(form)),
      });
      if (!res.ok) return null;
      const result = await res.json();
      return result.valid ? {} : (result.errors || {});
    } catch {
      return null;
    }
  }

  /**
   * Snapshot the form into a JSON-friendly payload, peeling off the
   * special fields (`_csrf`, `_method`, `_action`, `_locale`) and
   * skipping file inputs (validated separately on the real submit).
   *
   * @param {HTMLFormElement} form
   * @returns {CollectedFormData}
   */
  _collectFormData(form) {
    /** @type {Record<string, any>} */
    const data = {};
    let draft = false;
    /** @type {string|null} */
    let locale = null;

    for (const [key, value] of new FormData(form).entries()) {
      if (value instanceof File) continue;
      if (key === '_csrf' || key === '_method') continue;
      if (key === '_action') {
        if (value === 'save_draft') draft = true;
        continue;
      }
      if (key === '_locale') {
        locale = /** @type {string} */ (value);
        continue;
      }
      if (key in data) {
        data[key] = Array.isArray(data[key]) ? [...data[key], value] : [data[key], value];
      } else {
        data[key] = value;
      }
    }
    return { data, draft, locale };
  }

  /* ── Error UI ───────────────────────────────────────────────── */

  /**
   * Render inline errors next to fields and toast a summary.
   *
   * Some error keys are richtext node-attr errors of the form
   * `parent[type#index].attr` — those are dispatched to the
   * `crap-richtext` component for in-editor highlighting AND aggregated
   * onto the parent field as a fallback inline message.
   *
   * @param {Record<string, string>} errors
   */
  _showErrors(errors) {
    this._clearErrors();
    const { directErrors, nodeAttrErrors } = this._partitionErrors(errors);
    this._dispatchRichtextErrors(errors, nodeAttrErrors);
    this._mergeNodeAttrFallback(directErrors, nodeAttrErrors);

    let count = 0;
    for (const [field, message] of Object.entries(directErrors)) {
      if (this._renderFieldError(field, message)) count++;
    }
    if (count > 0) toast(t('validation.error_summary'), 'error');
  }

  /**
   * Partition the flat error map into two buckets:
   *  - `directErrors[field] = message`
   *  - `nodeAttrErrors[parentField] = [messages]` (aggregated for parents)
   *
   * @param {Record<string, string>} errors
   */
  _partitionErrors(errors) {
    /** @type {Record<string, string>} */
    const directErrors = {};
    /** @type {Record<string, string[]>} */
    const nodeAttrErrors = {};
    for (const [field, message] of Object.entries(errors)) {
      const parent = this._resolveNodeAttrParent(field);
      if (parent) {
        (nodeAttrErrors[parent] ??= []).push(message);
      } else {
        directErrors[field] = message;
      }
    }
    return { directErrors, nodeAttrErrors };
  }

  /**
   * Forward per-node error messages to the relevant `crap-richtext`
   * components so the editor can highlight the bad atoms.
   *
   * @param {Record<string, string>} errors Original flat error map (we
   *   need the original keys to extract `type#index`).
   * @param {Record<string, string[]>} nodeAttrErrors
   */
  _dispatchRichtextErrors(errors, nodeAttrErrors) {
    for (const parent of Object.keys(nodeAttrErrors)) {
      const wrapper = this.querySelector(`[data-field-name="${parent}"]`);
      const richtextEl = /** @type {any} */ (wrapper?.querySelector('crap-richtext'));
      if (!richtextEl || typeof richtextEl.markNodeErrors !== 'function') continue;

      const re = new RegExp(`^${escapeRegex(parent)}\\[([^\\]]*#[^\\]]*)\\]\\.`);
      /** @type {Record<string, string[]>} */
      const perNode = {};
      for (const [key, message] of Object.entries(errors)) {
        const m = key.match(re);
        if (m) (perNode[m[1]] ??= []).push(message);
      }
      richtextEl.markNodeErrors(perNode);
    }
  }

  /**
   * For richtext parents that don't have a direct error of their own,
   * render the aggregated node-attr messages as a single inline error
   * fallback message.
   *
   * @param {Record<string, string>} directErrors Mutated in place.
   * @param {Record<string, string[]>} nodeAttrErrors
   */
  _mergeNodeAttrFallback(directErrors, nodeAttrErrors) {
    for (const [parent, msgs] of Object.entries(nodeAttrErrors)) {
      if (!directErrors[parent]) directErrors[parent] = msgs.join('; ');
    }
  }

  /**
   * Render one inline `<p class="form__error">` next to a field, mark
   * the parent array row if any, and return whether the field wrapper
   * was found.
   *
   * @param {string} field
   * @param {string} message
   */
  _renderFieldError(field, message) {
    const wrapper = this.querySelector(`[data-field-name="${field}"]`);
    if (!wrapper) return false;

    // Replace any server-rendered error already there.
    wrapper.querySelector(':scope > .form__error')?.remove();
    wrapper.appendChild(h('p', {
      class: 'form__error',
      'data-validate-error': true,
      role: 'alert',
      text: message,
    }));

    this._markArrayRowErrors(wrapper);
    return true;
  }

  /**
   * If `wrapper` lives inside a `.form__array-row`, expand the row,
   * mark it as having errors, and add an error badge in the header.
   *
   * @param {Element} wrapper
   */
  _markArrayRowErrors(wrapper) {
    const row = wrapper.closest('.form__array-row');
    if (!row) return;
    row.classList.add('form__array-row--has-errors');
    row.classList.remove('form__array-row--collapsed');
    row.setAttribute('data-validate-row-error', '');

    const toggleBtn = row.querySelector('.form__array-row-toggle');
    toggleBtn?.setAttribute('aria-expanded', 'true');

    const header = row.querySelector('.form__array-row-header');
    if (!header || header.querySelector('.form__array-row-error-badge')) return;

    const badge = h('span', {
      class: 'form__array-row-error-badge',
      'data-validate-error': true,
    }, h('span', {
      class: ['material-symbols-outlined', 'icon--sm'],
      'aria-hidden': true,
      text: 'error',
    }));

    // Insert after the toggle button (so the badge sits next to the chevron).
    const toggle = header.querySelector('.form__array-row-toggle');
    if (toggle) toggle.after(badge);
  }

  /**
   * Whether `key` is a richtext node-attr error and, if so, what the
   * parent field name is.
   *
   * Examples: `content[cta#0].text` → `content`,
   *           `items[0][body][cta#1].url` → `items[0][body]`.
   *
   * @param {string} key
   * @returns {string|null}
   */
  _resolveNodeAttrParent(key) {
    const m = key.match(/^(.+?)\[[^\]]*#[^\]]*\]/);
    return m ? m[1] : null;
  }

  /** Strip every error indicator this component injected. */
  _clearErrors() {
    for (const el of this.querySelectorAll('[data-validate-error]')) el.remove();
    for (const row of this.querySelectorAll('[data-validate-row-error]')) {
      row.classList.remove('form__array-row--has-errors');
      row.removeAttribute('data-validate-row-error');
    }
    for (const el of /** @type {NodeListOf<HTMLElement & {clearNodeErrors?: () => void}>} */ (
      this.querySelectorAll('crap-richtext')
    )) {
      el.clearNodeErrors?.();
    }
  }
}

/** @param {string} str */
function escapeRegex(str) {
  return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

customElements.define('crap-validate-form', CrapValidateForm);
