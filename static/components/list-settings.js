/**
 * List settings — `<crap-list-settings>`.
 *
 * Toolbar component for collection list views. Provides:
 *   - **Column picker** drawer — pick which columns the list table shows.
 *   - **Filter builder** drawer — compose `where[field][op]=value` URL
 *     filters using per-type operator menus and value inputs.
 *
 * Both drawers borrow the page-singleton `<crap-drawer>` via
 * [`EV_DRAWER_REQUEST`](./events.js). The toolbar mounts a freshly
 * constructed `<crap-column-picker>` or `<crap-filter-builder>`
 * element into the drawer body and listens for the element's
 * completion event (`crap:column-picker-saved` /
 * `crap:filter-builder-applied`) to close the drawer.
 *
 * Field metadata + saved selections come from JSON data islands the
 * server renders into the page (`crap-column-options`,
 * `crap-filter-fields`).
 *
 * Light DOM — operates on the server-rendered list page and uses
 * HTMX-aware navigation for filter application.
 *
 * @module list-settings
 * @stability experimental
 */

import { clear } from './_internal/h.js';
import { t } from './_internal/i18n.js';
import { discoverSingleton } from './_internal/util/discover.js';
import { readDataIsland } from './_internal/util/json.js';
import { EV_DRAWER_REQUEST } from './events.js';

/**
 * @typedef {{
 *   open: (opts: { title: string }) => void,
 *   close: () => void,
 *   body: HTMLElement,
 * }} DrawerInstance
 */

class CrapListSettings extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {boolean} */
    this._searchWasActive = false;
    /** @type {((e: Event) => void)|null} */
    this._onBeforeRequest = null;
    /** @type {(() => void)|null} */
    this._onAfterSettle = null;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    this.addEventListener('click', (e) => this._onToolbarClick(e));
    this._setupSearchFocusPreservation();
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    if (this._onBeforeRequest)
      document.removeEventListener('htmx:beforeRequest', this._onBeforeRequest);
    if (this._onAfterSettle) document.removeEventListener('htmx:afterSettle', this._onAfterSettle);
  }

  /** Collection slug from the current URL, or `null` if not on a list page. */
  get _slug() {
    const m = window.location.pathname.match(/^\/admin\/collections\/([^/]+)\/?$/);
    return m ? m[1] : null;
  }

  /** @param {Event} e */
  _onToolbarClick(e) {
    if (!(e.target instanceof Element)) return;
    const btn = /** @type {HTMLElement|null} */ (e.target.closest('[data-action]'));
    if (!btn) return;
    const slug = this._slug;
    if (!slug) return;
    switch (btn.dataset.action) {
      case 'open-column-picker':
        this._openColumnPicker(slug);
        break;
      case 'open-filter-builder':
        this._openFilterBuilder(slug);
        break;
    }
  }

  /**
   * Mount a freshly constructed `<crap-column-picker>` into the drawer.
   * The element's connectedCallback builds the form; we listen for its
   * `crap:column-picker-saved` event to close the drawer.
   *
   * @param {string} slug
   */
  _openColumnPicker(slug) {
    const drawer = /** @type {DrawerInstance|null} */ (discoverSingleton(EV_DRAWER_REQUEST));
    if (!drawer) return;
    const options = readDataIsland(this, 'crap-column-options', []);

    const picker = document.createElement('crap-column-picker');
    picker.dataset.collection = slug;
    picker.dataset.options = JSON.stringify(options);
    picker.addEventListener('crap:column-picker-saved', () => drawer.close(), { once: true });

    drawer.open({ title: t('columns') });
    clear(drawer.body);
    drawer.body.appendChild(picker);
  }

  /**
   * Mount a freshly constructed `<crap-filter-builder>` into the
   * drawer. Listens for its `crap:filter-builder-applied` event to
   * close the drawer (the builder itself triggers the htmx-aware
   * navigation).
   *
   * @param {string} slug
   */
  _openFilterBuilder(slug) {
    const drawer = /** @type {DrawerInstance|null} */ (discoverSingleton(EV_DRAWER_REQUEST));
    if (!drawer) return;
    const fieldMetas = readDataIsland(this, 'crap-filter-fields', []);
    if (!fieldMetas.length) return;

    const builder = document.createElement('crap-filter-builder');
    builder.dataset.collection = slug;
    builder.dataset.fields = JSON.stringify(fieldMetas);
    builder.addEventListener('crap:filter-builder-applied', () => drawer.close(), { once: true });

    drawer.open({ title: t('filters') });
    clear(drawer.body);
    drawer.body.appendChild(builder);
  }

  /**
   * Restore focus + caret position to the list-view search input across
   * HTMX list swaps. Without this, the input loses focus on every
   * keystroke that triggers a server-side search refresh.
   */
  _setupSearchFocusPreservation() {
    this._onBeforeRequest = (e) => {
      const detail = /** @type {CustomEvent} */ (e).detail;
      if (detail.elt?.id === 'list-search-input') this._searchWasActive = true;
    };
    this._onAfterSettle = () => {
      if (!this._searchWasActive) return;
      this._searchWasActive = false;
      const input = /** @type {HTMLInputElement|null} */ (
        document.getElementById('list-search-input')
      );
      if (!input) return;
      input.focus();
      input.setSelectionRange(input.value.length, input.value.length);
    };
    document.addEventListener('htmx:beforeRequest', this._onBeforeRequest);
    document.addEventListener('htmx:afterSettle', this._onAfterSettle);
  }
}

customElements.define('crap-list-settings', CrapListSettings);
