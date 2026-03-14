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

import { t } from './i18n.js';

class CrapValidateForm extends HTMLElement {
  connectedCallback() {
    /** @type {boolean} */
    this._validated = false;

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
    if (this._onBeforeRequest) {
      document.body.removeEventListener('htmx:beforeRequest', this._onBeforeRequest);
    }
  }

  /**
   * Run validation via the JSON endpoint, then re-trigger submit if valid.
   * @param {HTMLFormElement} form
   */
  async _runValidation(form) {
    const url = this._validateUrl;
    if (!url) {
      // No validate URL — let form submit normally
      this._validated = true;
      this._retrigger(form);
      return;
    }

    // Collect form data (minus file inputs)
    const data = this._collectFormData(form);
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

      if (!res.ok) {
        window.CrapToast?.show(t('validation.server_error'), 'error');
        return;
      }

      const result = await res.json();

      if (result.valid) {
        // Clear any previous errors
        this._clearErrors();

        // Mark as validated and re-trigger the real submit
        this._validated = true;
        this._retrigger(form);
      } else {
        this._showErrors(result.errors || {});
      }
    } catch (err) {
      window.CrapToast?.show(t('validation.server_error'), 'error');
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

      data[key] = value;
    }

    return { data, draft, locale };
  }

  /**
   * Show validation errors inline next to their fields and as a toast.
   * @param {Record<string, string>} errors
   */
  _showErrors(errors) {
    this._clearErrors();

    let errorCount = 0;

    for (const [field, message] of Object.entries(errors)) {
      const wrapper = this.querySelector(`[data-field-name="${field}"]`);
      if (!wrapper) continue;

      // Remove any existing server-rendered error
      const existing = wrapper.querySelector('.form__error');
      if (existing) existing.remove();

      const errorEl = document.createElement('p');
      errorEl.className = 'form__error';
      errorEl.setAttribute('data-validate-error', '');
      errorEl.textContent = message;
      wrapper.appendChild(errorEl);
      errorCount++;
    }

    if (errorCount > 0) {
      window.CrapToast?.show(t('validation.error_summary'), 'error');
    }
  }

  /**
   * Clear all validation errors injected by this component.
   */
  _clearErrors() {
    const errors = this.querySelectorAll('[data-validate-error]');
    for (const el of errors) {
      el.remove();
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
    return m ? m[1] : null;
  }
}

customElements.define('crap-validate-form', CrapValidateForm);
