/**
 * Form UI state preservation — `<crap-scroll-restore>`.
 *
 * Snapshots scroll position, active tab indices, group/collapsible
 * collapsed states, and array row collapsed states before form submissions.
 * Restores them on the next page load.
 *
 * @module scroll
 */

const STORAGE_KEY = 'crap-form-state';

class CrapScrollRestore extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._pendingRedirect = false;
  }

  connectedCallback() {
    this._onBeforeRequest = /** @param {CustomEvent} e */ (e) => {
      if (e.detail.requestConfig.verb === 'get') return;
      this._pendingRedirect = true;
      this._saveFormState();
    };

    this._maybeRestore = () => {
      if (this._pendingRedirect) return;
      this._restoreFormState();
    };

    document.addEventListener('htmx:beforeRequest', this._onBeforeRequest);
    document.addEventListener('htmx:afterSettle', this._maybeRestore);

    // Also restore on initial connect (for full page loads after redirect)
    this._maybeRestore();
  }

  disconnectedCallback() {
    document.removeEventListener('htmx:beforeRequest', this._onBeforeRequest);
    document.removeEventListener('htmx:afterSettle', this._maybeRestore);
  }

  _saveFormState() {
    /** @type {{ url: string, scrollY: number, tabs: Object<string,string>, groups: Object<string,string>, rows: Object<string,string> }} */
    const state = {
      url: location.pathname,
      scrollY: window.scrollY,
      tabs: {},
      groups: {},
      rows: {},
    };

    // Active tab indices
    document.querySelectorAll('.form__tabs[data-tabs-name]').forEach(
      /** @param {HTMLElement} tabs */ (tabs) => {
        const name = tabs.getAttribute('data-tabs-name');
        const active = tabs.querySelector('.form__tabs-tab--active');
        if (name && active) {
          state.tabs[name] = active.getAttribute('data-tab-index');
        }
      }
    );

    // Group/collapsible collapsed states
    document.querySelectorAll('[data-collapsible][data-group-name]').forEach(
      /** @param {HTMLElement} fieldset */ (fieldset) => {
        const name = fieldset.getAttribute('data-group-name');
        if (!name) return;
        const cls = fieldset.classList.contains('form__collapsible')
          ? 'form__collapsible--collapsed'
          : 'form__group--collapsed';
        state.groups[name] = fieldset.classList.contains(cls) ? '1' : '0';
      }
    );

    // Array/block row collapsed states
    document.querySelectorAll('.form__array[data-field-name]').forEach(
      /** @param {HTMLElement} fieldset */ (fieldset) => {
        const fieldName = fieldset.getAttribute('data-field-name');
        if (!fieldName) return;
        fieldset.querySelectorAll(':scope > .form__array-rows > .form__array-row').forEach(
          /** @param {HTMLElement} row */ (row) => {
            const idx = row.getAttribute('data-row-index');
            if (idx == null) return;
            const key = fieldName + '[' + idx + ']';
            state.rows[key] = row.classList.contains('form__array-row--collapsed') ? '1' : '0';
          }
        );
      }
    );

    try {
      sessionStorage.setItem(STORAGE_KEY, JSON.stringify(state));
    } catch { /* private browsing or quota exceeded */ }
  }

  _restoreFormState() {
    let raw;
    try {
      raw = sessionStorage.getItem(STORAGE_KEY);
      sessionStorage.removeItem(STORAGE_KEY);
    } catch { return; }
    if (!raw) return;

    /** @type {{ url: string, scrollY: number, tabs: Object<string,string>, groups: Object<string,string>, rows?: Object<string,string> }} */
    let state;
    try { state = JSON.parse(raw); } catch { return; }
    if (state.url !== location.pathname) return;

    // Restore tabs
    for (const [name, index] of Object.entries(state.tabs)) {
      const tabs = document.querySelector(`.form__tabs[data-tabs-name="${name}"]`);
      if (!tabs) continue;
      const btn = tabs.querySelector(`.form__tabs-tab[data-tab-index="${index}"]`);
      const panel = tabs.querySelector(`[data-tab-panel="${index}"]`);
      if (!btn || !panel) continue;

      tabs.querySelectorAll('.form__tabs-tab').forEach((t) => {
        t.classList.remove('form__tabs-tab--active');
        t.setAttribute('aria-selected', 'false');
      });
      tabs.querySelectorAll('.form__tabs-panel').forEach((p) => p.classList.add('form__tabs-panel--hidden'));
      btn.classList.add('form__tabs-tab--active');
      btn.setAttribute('aria-selected', 'true');
      panel.classList.remove('form__tabs-panel--hidden');
    }

    // Restore groups
    for (const [name, val] of Object.entries(state.groups)) {
      const fieldset = document.querySelector(`[data-collapsible][data-group-name="${name}"]`);
      if (!fieldset) continue;
      const cls = fieldset.classList.contains('form__collapsible')
        ? 'form__collapsible--collapsed'
        : 'form__group--collapsed';
      fieldset.classList.toggle(cls, val === '1');
    }

    // Restore array/block row states
    if (state.rows) {
      for (const [key, val] of Object.entries(state.rows)) {
        const match = key.match(/^(.+)\[(\d+)\]$/);
        if (!match) continue;
        const [, fieldName, idx] = match;
        const fieldset = document.querySelector(`.form__array[data-field-name="${fieldName}"]`);
        if (!fieldset) continue;
        const row = fieldset.querySelector(`:scope > .form__array-rows > .form__array-row[data-row-index="${idx}"]`);
        if (row) row.classList.toggle('form__array-row--collapsed', val === '1');
      }
    }

    // Restore scroll (after DOM updates so layout is settled)
    if (state.scrollY != null) {
      requestAnimationFrame(() => window.scrollTo(0, state.scrollY));
    }
  }
}

customElements.define('crap-scroll-restore', CrapScrollRestore);
