/**
 * Form UI state preservation — `<crap-scroll-restore>`.
 *
 * Snapshots scroll position, active tab indices, group/collapsible
 * collapsed states, and array-row collapsed states **before** form
 * submissions. Restores them on the next page load (full reload or
 * HTMX swap).
 *
 * @module scroll
 * @stability stable
 */

import { getHttpVerb } from './_internal/util/htmx.js';

const STORAGE_KEY = 'crap-form-state';

/** Modifier classes that mark "collapsed" on the two collapsible kinds. */
const COLLAPSED_CLASS_FOR = {
  form__collapsible: 'form__collapsible--collapsed',
  form__group: 'form__group--collapsed',
};

/**
 * @typedef {{
 *   url: string,
 *   scrollY: number,
 *   tabs: Record<string, string>,
 *   groups: Record<string, string>,
 *   rows: Record<string, string>,
 * }} FormState
 */

class CrapScrollRestore extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {boolean} */
    this._pendingRedirect = false;
    /** @type {((e: Event) => void)|null} */
    this._onBeforeRequest = null;
    /** @type {(() => void)|null} */
    this._onAfterSettle = null;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    this._onBeforeRequest = (e) => {
      if (getHttpVerb(e) === 'GET') return;
      this._pendingRedirect = true;
      this._saveFormState();
    };
    this._onAfterSettle = () => {
      if (this._pendingRedirect) return;
      this._restoreFormState();
    };

    document.addEventListener('htmx:beforeRequest', this._onBeforeRequest);
    document.addEventListener('htmx:afterSettle', this._onAfterSettle);

    // Restore on initial connect (full-page load after a redirect).
    this._restoreFormState();
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    if (this._onBeforeRequest)
      document.removeEventListener('htmx:beforeRequest', this._onBeforeRequest);
    if (this._onAfterSettle) document.removeEventListener('htmx:afterSettle', this._onAfterSettle);
  }

  /* ── Save ───────────────────────────────────────────────────── */

  _saveFormState() {
    /** @type {FormState} */
    const state = {
      url: location.pathname,
      scrollY: window.scrollY,
      tabs: this._snapshotTabs(),
      groups: this._snapshotGroups(),
      rows: this._snapshotRows(),
    };
    try {
      sessionStorage.setItem(STORAGE_KEY, JSON.stringify(state));
    } catch {
      /* private browsing or quota exceeded */
    }
  }

  /** @returns {Record<string, string>} */
  _snapshotTabs() {
    /** @type {Record<string, string>} */
    const tabs = {};
    for (const el of /** @type {NodeListOf<HTMLElement>} */ (
      document.querySelectorAll('.form__tabs[data-tabs-name]')
    )) {
      const name = el.getAttribute('data-tabs-name');
      const active = el.querySelector('.form__tabs-tab--active');
      const idx = active?.getAttribute('data-tab-index');
      if (name && idx != null) tabs[name] = idx;
    }
    return tabs;
  }

  /** @returns {Record<string, string>} */
  _snapshotGroups() {
    /** @type {Record<string, string>} */
    const groups = {};
    for (const fs of /** @type {NodeListOf<HTMLElement>} */ (
      document.querySelectorAll('[data-collapsible][data-group-name]')
    )) {
      const name = fs.getAttribute('data-group-name');
      if (!name) continue;
      const cls = collapsedClassFor(fs);
      groups[name] = fs.classList.contains(cls) ? '1' : '0';
    }
    return groups;
  }

  /** @returns {Record<string, string>} */
  _snapshotRows() {
    /** @type {Record<string, string>} */
    const rows = {};
    for (const fs of /** @type {NodeListOf<HTMLElement>} */ (
      document.querySelectorAll('.form__array[data-field-name]')
    )) {
      const fieldName = fs.getAttribute('data-field-name');
      if (!fieldName) continue;
      for (const row of /** @type {NodeListOf<HTMLElement>} */ (
        fs.querySelectorAll(':scope > .form__array-rows > .form__array-row')
      )) {
        const idx = row.getAttribute('data-row-index');
        if (idx == null) continue;
        rows[`${fieldName}[${idx}]`] = row.classList.contains('form__array-row--collapsed')
          ? '1'
          : '0';
      }
    }
    return rows;
  }

  /* ── Restore ────────────────────────────────────────────────── */

  _restoreFormState() {
    const state = this._consumeState();
    if (!state || state.url !== location.pathname) return;

    this._restoreTabs(state.tabs);
    this._restoreGroups(state.groups);
    if (state.rows) this._restoreRows(state.rows);
    this._restoreScroll(state.scrollY);
  }

  /**
   * Read the saved state from sessionStorage and remove it (one-shot).
   * @returns {FormState|null}
   */
  _consumeState() {
    let raw;
    try {
      raw = sessionStorage.getItem(STORAGE_KEY);
      sessionStorage.removeItem(STORAGE_KEY);
    } catch {
      return null;
    }
    if (!raw) return null;
    try {
      return JSON.parse(raw);
    } catch {
      return null;
    }
  }

  /** @param {Record<string, string>} tabs */
  _restoreTabs(tabs) {
    for (const [name, index] of Object.entries(tabs)) {
      const tabsEl = document.querySelector(`.form__tabs[data-tabs-name="${name}"]`);
      if (!tabsEl) continue;
      const btn = tabsEl.querySelector(`.form__tabs-tab[data-tab-index="${index}"]`);
      const panel = tabsEl.querySelector(`[data-tab-panel="${index}"]`);
      if (!btn || !panel) continue;

      for (const tab of tabsEl.querySelectorAll('.form__tabs-tab')) {
        tab.classList.remove('form__tabs-tab--active');
        tab.setAttribute('aria-selected', 'false');
      }
      for (const p of tabsEl.querySelectorAll('.form__tabs-panel')) {
        p.classList.add('form__tabs-panel--hidden');
      }
      btn.classList.add('form__tabs-tab--active');
      btn.setAttribute('aria-selected', 'true');
      panel.classList.remove('form__tabs-panel--hidden');
    }
  }

  /** @param {Record<string, string>} groups */
  _restoreGroups(groups) {
    for (const [name, val] of Object.entries(groups)) {
      const fs = document.querySelector(`[data-collapsible][data-group-name="${name}"]`);
      if (!(fs instanceof HTMLElement)) continue;
      fs.classList.toggle(collapsedClassFor(fs), val === '1');
    }
  }

  /** @param {Record<string, string>} rows */
  _restoreRows(rows) {
    for (const [key, val] of Object.entries(rows)) {
      const m = key.match(/^(.+)\[(\d+)\]$/);
      if (!m) continue;
      const [, fieldName, idx] = m;
      const fs = document.querySelector(`.form__array[data-field-name="${fieldName}"]`);
      const row = fs?.querySelector(
        `:scope > .form__array-rows > .form__array-row[data-row-index="${idx}"]`,
      );
      row?.classList.toggle('form__array-row--collapsed', val === '1');
    }
  }

  /** @param {number} scrollY */
  _restoreScroll(scrollY) {
    if (scrollY == null) return;
    // Defer to next frame so DOM updates above have settled.
    requestAnimationFrame(() => window.scrollTo(0, scrollY));
  }
}

/**
 * Pick the `--collapsed` modifier class that applies to `el` based on
 * its base class.
 *
 * @param {HTMLElement} el
 */
function collapsedClassFor(el) {
  return el.classList.contains('form__collapsible')
    ? COLLAPSED_CLASS_FOR.form__collapsible
    : COLLAPSED_CLASS_FOR.form__group;
}

customElements.define('crap-scroll-restore', CrapScrollRestore);
