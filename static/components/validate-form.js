/**
 * <crap-validate-form> — Pre-submit validation for upload forms.
 *
 * Wraps a form that contains file uploads. On submit, intercepts the HTMX
 * request, validates non-file fields via a JSON endpoint first, and only
 * lets the real submission (with file) proceed after validation passes.
 *
 * This prevents uploaded files from being lost when validation fails,
 * since browsers cannot re-populate file inputs.
 *
 * @example
 * <crap-validate-form validate-url="/admin/collections/media/validate">
 *   <form id="edit-form" ...>
 *     ...
 *   </form>
 * </crap-validate-form>
 *
 * @module validate-form
 */

import { h } from './h.js';
import { t } from './i18n.js';

class CrapValidateForm extends HTMLElement {
  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    /** @type {boolean} */
    this._validated = false;

    /** @type {boolean} */
    this._validating = false;

    /** @type {string} */
    this._validateUrl = this.getAttribute('validate-url') || '';

    /**
     * Intercept HTMX requests from the child form.
     * On first fire: cancel and validate. If valid, re-trigger.
     * @param {CustomEvent} evt
     */
    this._onBeforeRequest = (evt) => {
      const form = this.querySelector('#edit-form');
      if (!form) return;

      // Only intercept requests from our form
      if (evt.detail.elt !== form) return;

      // If we already validated, let it through
      if (this._validated) {
        this._validated = false;
        return;
      }

      // Cancel the HTMX request
      evt.preventDefault();

      this._runValidation(form);
    };

    document.body.addEventListener('htmx:beforeRequest', this._onBeforeRequest);
  }

  disconnectedCallback() {
    this._connected = false;
    if (this._onBeforeRequest) {
      document.body.removeEventListener('htmx:beforeRequest', this._onBeforeRequest);
      this._onBeforeRequest = null;
    }
  }

  /**
   * Run validation via the JSON endpoint and return the error map.
   * Returns `{}` when valid, an error map on invalid, or `null` on network error.
   * @returns {Promise<Record<string, string>|null>}
   */
  async getValidationErrors() {
    const url = this._validateUrl;
    if (!url) return {};

    const form = this.querySelector('#edit-form');
    if (!form) return {};

    const data = this._collectFormData(/** @type {HTMLFormElement} */ (form));
    const csrf = this._getCsrf();

    /** @type {Record<string, string>} */
    const headers = { 'Content-Type': 'application/json' };
    if (csrf) headers['X-CSRF-Token'] = csrf;

    try {
      const res = await fetch(url, {
        method: 'POST',
        headers,
        body: JSON.stringify(data),
      });

      if (!res.ok) return null;

      const result = await res.json();
      return result.valid ? {} : (result.errors || {});
    } catch {
      return null;
    }
  }

  /**
   * Run validation via the JSON endpoint, then re-trigger submit if valid.
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
        document.dispatchEvent(new CustomEvent('crap:toast', { detail: { message: t('validation.server_error'), type: 'error' } }));
        return;
      }

      if (Object.keys(errors).length === 0) {
        this._clearErrors();
        this._validated = true;
        this._retrigger(form);
      } else {
        this._showErrors(errors);
      }
    } finally {
      this._validating = false;
    }
  }

  /**
   * Collect form data as a JSON-friendly object, excluding file inputs
   * and internal fields (_csrf, _method).
   * @param {HTMLFormElement} form
   * @returns {{ data: Record<string, any>, draft: boolean, locale: string|null }}
   */
  _collectFormData(form) {
    const formData = new FormData(form);
    /** @type {Record<string, any>} */
    const data = {};
    let draft = false;
    /** @type {string|null} */
    let locale = null;

    for (const [key, value] of formData.entries()) {
      // Skip file inputs, CSRF, method override
      if (value instanceof File) continue;
      if (key === '_csrf' || key === '_method') continue;

      // Extract special fields
      if (key === '_action') {
        if (value === 'save_draft') draft = true;
        continue;
      }
      if (key === '_locale') {
        locale = /** @type {string} */ (value);
        continue;
      }

      if (key in data) {
        data[key] = Array.isArray(data[key])
          ? [...data[key], value]
          : [data[key], value];
      } else {
        data[key] = value;
      }
    }

    return { data, draft, locale };
  }

  /**
   * Show validation errors inline next to their fields and as a toast.
   * Node attr errors (e.g. `content[cta#0].text`) are aggregated onto the
   * parent richtext field wrapper (`content`).
   * @param {Record<string, string>} errors
   */
  _showErrors(errors) {
    this._clearErrors();

    // Aggregate node attr errors onto their parent field.
    // Keys like "content[cta#0].text" → parent "content",
    // "items[0][body][cta#1].url" → parent "items[0][body]".
    /** @type {Record<string, string[]>} */
    const nodeAttrErrors = {};
    /** @type {Record<string, string>} */
    const directErrors = {};

    for (const [field, message] of Object.entries(errors)) {
      const nodeAttrParent = this._resolveNodeAttrParent(field);
      if (nodeAttrParent) {
        (nodeAttrErrors[nodeAttrParent] ??= []).push(message);
      } else {
        directErrors[field] = message;
      }
    }

    // Pass structured per-node errors to richtext components
    for (const [parent, msgs] of Object.entries(nodeAttrErrors)) {
      const wrapper = this.querySelector(`[data-field-name="${parent}"]`);
      if (!wrapper) continue;
      const richtextEl = wrapper.querySelector('crap-richtext');
      if (richtextEl && typeof richtextEl.markNodeErrors === 'function') {
        /** @type {Record<string, string[]>} */
        const perNode = {};
        for (const [key, message] of Object.entries(errors)) {
          // Match keys like "content[cta#0].text" where parent is "content"
          const m = key.match(new RegExp(`^${parent.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\[([^\\]]*#[^\\]]*)\\]\\.`));
          if (m) {
            (perNode[m[1]] ??= []).push(message);
          }
        }
        richtextEl.markNodeErrors(perNode);
      }
    }

    // Merge aggregated node attr errors (only if no direct error for that field)
    for (const [parent, msgs] of Object.entries(nodeAttrErrors)) {
      if (!directErrors[parent]) {
        directErrors[parent] = msgs.join('; ');
      }
    }

    let errorCount = 0;

    for (const [field, message] of Object.entries(directErrors)) {
      const wrapper = this.querySelector(`[data-field-name="${field}"]`);
      if (!wrapper) continue;

      // Remove any existing server-rendered error
      const existing = wrapper.querySelector(':scope > .form__error');
      if (existing) existing.remove();

      const errorEl = document.createElement('p');
      errorEl.className = 'form__error';
      errorEl.setAttribute('data-validate-error', '');
      errorEl.setAttribute('role', 'alert');
      errorEl.textContent = message;
      wrapper.appendChild(errorEl);
      errorCount++;

      // Mark parent array/blocks row as having errors and expand it
      const row = wrapper.closest('.form__array-row');
      if (row) {
        row.classList.add('form__array-row--has-errors');
        row.classList.remove('form__array-row--collapsed');
        row.setAttribute('data-validate-row-error', '');
        const toggleBtn = row.querySelector('.form__array-row-toggle');
        if (toggleBtn) toggleBtn.setAttribute('aria-expanded', 'true');
        // Add error badge if not already present
        const header = row.querySelector('.form__array-row-header');
        if (header && !header.querySelector('.form__array-row-error-badge')) {
          const badge = h('span', {
            class: 'form__array-row-error-badge',
            'data-validate-error': '',
          }, h('span', {
            class: ['material-symbols-outlined', 'icon--sm'],
            'aria-hidden': 'true',
            text: 'error',
          }));
          // Insert after the toggle button
          const toggle = header.querySelector('.form__array-row-toggle');
          if (toggle) toggle.after(badge);
        }
      }
    }

    if (errorCount > 0) {
      document.dispatchEvent(new CustomEvent('crap:toast', { detail: { message: t('validation.error_summary'), type: 'error' } }));
    }
  }

  /**
   * Check if an error key is a richtext node attr error and return the parent
   * field name. Node attr keys contain `#` inside a bracket segment, e.g.
   * `content[cta#0].text` → `content`, `items[0][body][cta#1].url` → `items[0][body]`.
   * @param {string} key
   * @returns {string|null} parent field name, or null if not a node attr key
   */
  _resolveNodeAttrParent(key) {
    // Find the bracket segment containing "#" (e.g. [cta#0])
    const match = key.match(/^(.+?)\[[^\]]*#[^\]]*\]/);
    return match ? match[1] : null;
  }

  /**
   * Clear all validation errors injected by this component.
   */
  _clearErrors() {
    const errors = this.querySelectorAll('[data-validate-error]');
    for (const el of errors) {
      el.remove();
    }
    // Clear row error state added by validation
    const errorRows = this.querySelectorAll('[data-validate-row-error]');
    for (const row of errorRows) {
      row.classList.remove('form__array-row--has-errors');
      row.removeAttribute('data-validate-row-error');
    }
    // Clear richtext node error highlighting
    const richtextEls = this.querySelectorAll('crap-richtext');
    for (const el of richtextEls) {
      if (typeof el.clearNodeErrors === 'function') {
        el.clearNodeErrors();
      }
    }
  }

  /**
   * Re-trigger the form submission via HTMX.
   * @param {HTMLFormElement} form
   */
  _retrigger(form) {
    // Use htmx.trigger to re-fire the form submission
    if (window.htmx) {
      window.htmx.trigger(form, 'submit');
    } else {
      form.requestSubmit();
    }
  }

  /**
   * Read the CSRF cookie value.
   * @returns {string|null}
   */
  _getCsrf() {
    const m = document.cookie.match(/(?:^|; )crap_csrf=([^;]*)/);
    if (!m) return null;
    try { return decodeURIComponent(m[1]); } catch { return m[1]; }
  }
}

customElements.define('crap-validate-form', CrapValidateForm);
