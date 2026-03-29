/**
 * Display conditions — `<crap-conditions>`.
 *
 * Supports two modes:
 * - Client-side (data-condition): JSON condition table, evaluated instantly.
 * - Server-side (data-condition-ref): Lua function ref, debounced POST.
 *
 * @module conditions
 */

class CrapConditions extends HTMLElement {
  connectedCallback() {
    // Idempotency guard: skip re-init on DOM reconnection
    if (this._initialized) return;
    this._initialized = true;

    /** @type {number|null} */
    this._serverTimer = null;
    /** @type {AbortController|null} */
    this._serverAbort = null;
    /** @type {Array<{el: Element, type: string, fn: Function}>} */
    this._clientListeners = [];
    this._init();
  }

  disconnectedCallback() {
    if (this._serverTimer) clearTimeout(this._serverTimer);
    if (this._serverAbort) this._serverAbort.abort();
    if (this._debouncedServer) {
      const form = this._getForm();
      if (form) {
        form.removeEventListener('input', this._debouncedServer);
        form.removeEventListener('change', this._debouncedServer);
      }
    }
    for (const { el, type, fn } of this._clientListeners) {
      el.removeEventListener(type, fn);
    }
    this._clientListeners = [];
  }

  /**
   * @returns {HTMLFormElement|null}
   */
  _getForm() {
    return /** @type {HTMLFormElement|null} */ (
      this.querySelector('form') || this.closest('form')
    );
  }

  _init() {
    const form = this._getForm();
    if (!form) return;

    const clientFields = this.querySelectorAll('[data-condition]');
    const serverFields = this.querySelectorAll('[data-condition-ref]');
    if (clientFields.length === 0 && serverFields.length === 0) return;

    // --- Client-side conditions (instant) ---

    /** @type {Set<string>} */
    const watchedFields = new Set();
    clientFields.forEach((el) => {
      try {
        const cond = JSON.parse(/** @type {HTMLElement} */ (el).dataset.condition);
        this._extractWatchedFields(cond, watchedFields);
      } catch { /* skip malformed JSON */ }
    });

    const runClient = () => {
      const data = this._collectFormData(form);
      clientFields.forEach((el) => {
        try {
          const cond = JSON.parse(/** @type {HTMLElement} */ (el).dataset.condition);
          el.classList.toggle('form__field--hidden', !this._evaluate(cond, data));
        } catch { /* skip */ }
      });
    };

    watchedFields.forEach((fieldName) => {
      const input = form.querySelector('[name="' + CSS.escape(fieldName) + '"]');
      if (input) {
        input.addEventListener('input', runClient);
        input.addEventListener('change', runClient);
        this._clientListeners.push({ el: input, type: 'input', fn: runClient });
        this._clientListeners.push({ el: input, type: 'change', fn: runClient });
      }
    });

    // --- Server-side conditions (debounced) ---

    if (serverFields.length > 0) {
      const slug = this.getAttribute('collection') || form.dataset.collectionSlug || '';

      const runServer = () => {
        const data = this._collectFormData(form);
        /** @type {Object<string, string>} */
        const refs = {};
        serverFields.forEach((el) => {
          const name = /** @type {HTMLElement} */ (el).dataset.fieldName;
          const ref = /** @type {HTMLElement} */ (el).dataset.conditionRef;
          if (name && ref) refs[name] = ref;
        });

        const csrf = this._getCsrf();
        /** @type {Record<string, string>} */
        const headers = { 'Content-Type': 'application/json' };
        if (csrf) headers['X-CSRF-Token'] = csrf;

        // Cancel any in-flight request so stale responses don't overwrite
        // the result of a newer evaluation.
        if (this._serverAbort) this._serverAbort.abort();
        this._serverAbort = new AbortController();

        fetch('/admin/collections/' + slug + '/evaluate-conditions', {
          method: 'POST',
          headers,
          body: JSON.stringify({ form_data: data, conditions: refs }),
          signal: this._serverAbort.signal,
        })
        .then((r) => r.json())
        .then((result) => {
          for (const fieldName in result) {
            const el = this.querySelector(
              '[data-field-name="' + CSS.escape(fieldName) + '"][data-condition-ref]'
            );
            if (el) el.classList.toggle('form__field--hidden', !result[fieldName]);
          }
        })
        .catch(() => { /* silent fail — keep current visibility */ });
      };

      this._debouncedServer = () => {
        clearTimeout(this._serverTimer);
        this._serverTimer = setTimeout(runServer, 300);
      };

      form.addEventListener('input', this._debouncedServer);
      form.addEventListener('change', this._debouncedServer);
    }
  }

  /**
   * @param {*} val
   * @returns {boolean}
   */
  _conditionIsTruthy(val) {
    if (val == null || val === '' || val === false || val === 0) return false;
    if (Array.isArray(val)) return val.length > 0;
    return true;
  }

  /**
   * @param {Object|Array} condition
   * @param {Object} formData
   * @returns {boolean}
   */
  _evaluate(condition, formData) {
    if (Array.isArray(condition)) {
      return condition.every((c) => this._evaluate(c, formData));
    }
    let fieldVal = formData[condition.field];
    if (fieldVal === undefined) fieldVal = '';
    if ('equals' in condition) return fieldVal === condition.equals;
    if ('not_equals' in condition) return fieldVal !== condition.not_equals;
    if ('in' in condition) return condition['in'].includes(fieldVal);
    if ('not_in' in condition) return !condition['not_in'].includes(fieldVal);
    if (condition.is_truthy) return this._conditionIsTruthy(fieldVal);
    if (condition.is_falsy) return !this._conditionIsTruthy(fieldVal);
    return true;
  }

  /**
   * @param {HTMLFormElement} form
   * @returns {Object<string, string>}
   */
  _collectFormData(form) {
    const data = {};
    const fd = new FormData(form);
    for (const [key, val] of fd.entries()) {
      if (key.startsWith('_')) continue;
      data[key] = /** @type {string} */ (val);
    }
    form.querySelectorAll('input[type="checkbox"]').forEach(
      /** @param {HTMLInputElement} cb */ (cb) => {
        if (!cb.name.startsWith('_') && !(cb.name in data)) {
          data[cb.name] = cb.checked ? 'on' : '';
        }
      }
    );
    return data;
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

  /**
   * @param {Object|Array} condition
   * @param {Set<string>} set
   */
  _extractWatchedFields(condition, set) {
    if (Array.isArray(condition)) {
      condition.forEach((c) => this._extractWatchedFields(c, set));
    } else if (condition && condition.field) {
      set.add(condition.field);
    }
  }
}

customElements.define('crap-conditions', CrapConditions);
