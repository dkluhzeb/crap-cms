/**
 * Relationship / upload field — `<crap-relationship-search>`.
 *
 * Light-DOM custom element that replaces a `<select>` with a debounced
 * search input + dropdown (and chips for `has-many`). Supports
 * has-one / has-many, polymorphic targets, an inline browse drawer,
 * and inline-create panel integration.
 *
 * @attr collection   Target collection slug (primary / first).
 * @attr field-name   Form field name for the hidden input.
 * @attr field-type   `"relationship"` (default) | `"upload"`.
 * @attr has-many     Multi-select (chips).
 * @attr polymorphic  Multi-collection.
 * @attr collections  JSON array of collection slugs (when polymorphic).
 * @attr selected     JSON array of `{id, label, collection?}` for pre-selected items.
 * @attr picker       `"drawer"` to enable the browse-drawer UI.
 * @attr required     Required field.
 * @attr readonly     Readonly field.
 * @attr data-error   Boolean attribute for error styling.
 *
 * @module relationship-search
 * @stability experimental
 */

import { css } from './_internal/css.js';
import { clear, h } from './_internal/h.js';
import { t } from './_internal/i18n.js';
import { EV_CHANGE, EV_CREATE_PANEL_REQUEST, EV_DRAWER_REQUEST, EV_PICK } from './events.js';

/** Debounce window for the inline search input. */
const SEARCH_DEBOUNCE_MS = 250;
/** Debounce window for the drawer-picker search input. */
const DRAWER_DEBOUNCE_MS = 300;
/** Page size for drawer-picker results. */
const DRAWER_PAGE_SIZE = 24;
/** ms to ignore focus events after a programmatic pick. */
const SUPPRESS_FOCUS_MS = 300;
/** ms to wait after blur before closing the dropdown (lets click handlers fire first). */
const BLUR_CLOSE_MS = 200;

/**
 * @typedef {{
 *   id: string,
 *   label: string,
 *   collection?: string,
 *   thumbnail_url?: string,
 *   filename?: string,
 *   is_image?: boolean,
 * }} Item
 */

const sheet = css`
  crap-relationship-search {
    position: relative;
    display: block;
  }
  .relationship-search__input-wrapper { position: relative; }
  .relationship-search__input-row {
    display: flex;
    gap: var(--space-xs2);
    align-items: stretch;
  }
  .relationship-search__input-row .relationship-search__input-wrapper {
    flex: 1;
    min-width: 0;
  }
  .relationship-search__browse {
    all: unset;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: var(--control-sm);
    flex-shrink: 0;
    border: 1px solid var(--border-primary, #d9d9d9);
    border-radius: var(--radius-sm, 4px);
    background: var(--surface-primary, #fff);
    color: var(--text-secondary, rgba(0, 0, 0, 0.65));
    cursor: pointer;
    transition: border-color var(--transition-fast, 0.15s), color var(--transition-fast, 0.15s);
  }
  .relationship-search__browse:hover {
    border-color: var(--color-primary, #6366f1);
    color: var(--color-primary, #6366f1);
  }
  .relationship-search__input {
    width: 100%;
    padding: var(--space-sm) var(--space-md);
    border: 1px solid var(--border-primary);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    background: var(--surface-primary);
    color: var(--text-primary);
    transition: border-color var(--transition-fast);
    box-sizing: border-box;
  }
  .relationship-search__input:focus {
    outline: none;
    border-color: var(--color-primary);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--color-primary) 15%, transparent);
  }
  .relationship-search__clear {
    position: absolute;
    right: var(--space-sm);
    top: 50%;
    transform: translateY(-50%);
    background: none;
    border: none;
    cursor: pointer;
    color: var(--text-tertiary);
    font-size: var(--icon-sm);
    padding: var(--space-2xs) var(--space-xs);
    line-height: 1;
  }
  .relationship-search__clear:hover { color: var(--text-primary); }
  .relationship-search__dropdown {
    position: absolute;
    z-index: 100;
    top: 100%;
    left: 0;
    right: 0;
    max-height: var(--dropdown-max-height);
    overflow-y: auto;
    background: var(--surface-primary);
    border: 1px solid var(--border-primary);
    border-top: none;
    border-radius: 0 0 var(--radius-sm) var(--radius-sm);
    box-shadow: var(--shadow-md);
  }
  .relationship-search__option {
    padding: var(--space-sm) var(--space-md);
    cursor: pointer;
    font-size: var(--text-sm);
    color: var(--text-primary);
  }
  .relationship-search__option:hover,
  .relationship-search__option--active {
    background: var(--surface-hover);
  }
  .relationship-search__option--selected {
    color: var(--text-tertiary);
    font-style: italic;
  }
  .relationship-search__empty {
    padding: var(--space-sm) var(--space-md);
    color: var(--text-tertiary);
    font-size: var(--text-sm);
    font-style: italic;
  }
  .relationship-search__tags {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-xs);
    padding: var(--space-xs) var(--space-sm);
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    background: var(--surface-primary);
    min-height: var(--input-height);
    cursor: text;
  }
  .relationship-search__tags:focus-within {
    border-color: var(--accent-primary);
    box-shadow: 0 0 0 2px var(--accent-primary-bg, rgba(59, 130, 246, 0.1));
  }
  .relationship-search__tags--error { border-color: var(--color-danger); }
  .relationship-search__tags--has-items .relationship-search__tags-input {
    margin-left: var(--space-xs);
  }
  .relationship-search__tags-input {
    flex: 1 1 calc(var(--base) * 20);
    min-width: calc(var(--base) * 20);
    height: auto;
    border: none;
    outline: none;
    background: transparent;
    box-shadow: none;
    font-size: var(--text-sm);
    font-family: inherit;
    padding: var(--space-xs) 0;
    color: var(--text-primary);
  }
  .relationship-search__tags-input:focus {
    border: none;
    box-shadow: none;
  }
  .relationship-search__tags-input::placeholder { color: var(--text-tertiary); }
  /* Chip cluster styling lives on the <crap-pill-list> atom — see
     pill-list.js. That component injects its own stylesheet onto
     document.adoptedStyleSheets on first connect, so the chips
     render correctly regardless of which host mounts them. */
  .relationship-search__group-header {
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
    padding: var(--space-xs2) var(--space-sm2) var(--space-xs);
    border-bottom: 1px solid var(--border-color);
  }

  /* ── Drawer picker ──────────────────────────────────────────── */
  .rs-drawer__search {
    width: 100%;
    box-sizing: border-box;
    padding: var(--space-sm, 8px) var(--space-md, 12px);
    border: 1px solid var(--border-color, #e5e7eb);
    border-radius: var(--radius-md, 6px);
    font-size: var(--text-sm, 0.875rem);
    margin-bottom: var(--space-md, 12px);
    background: var(--input-bg, #fff);
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
  }
  .rs-drawer__results--grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: var(--space-md, 10px);
  }
  .rs-drawer__results--list {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs, 4px);
  }
  .rs-drawer__load-more {
    width: 100%;
    padding: var(--space-sm, 8px);
    margin-top: var(--space-md, 12px);
    border: 1px solid var(--border-color, #e5e7eb);
    border-radius: var(--radius-md, 6px);
    background: transparent;
    cursor: pointer;
    font-size: var(--text-sm, 0.875rem);
    color: var(--text-secondary, rgba(0, 0, 0, 0.65));
  }
  .rs-drawer__card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-sm, 6px);
    padding: var(--space-md, 10px);
    border: 2px solid var(--border-color, #e5e7eb);
    border-radius: var(--radius-md, 6px);
    background: var(--surface-primary, #fff);
    cursor: pointer;
    transition: border-color var(--transition-fast, 0.15s), background var(--transition-fast, 0.15s);
    min-height: 100px;
    position: relative;
    overflow: hidden;
  }
  .rs-drawer__card:hover { border-color: var(--color-primary, #6366f1); }
  .rs-drawer__card--selected {
    border-color: var(--color-primary, #6366f1);
    background: var(--color-primary-bg, rgba(99, 102, 241, 0.08));
  }
  .rs-drawer__card-img {
    width: 100%;
    height: 80px;
    object-fit: contain;
    border-radius: var(--radius-sm, 4px);
  }
  .rs-drawer__card-icon {
    font-size: var(--control-lg, 2.25rem);
    color: var(--text-tertiary, rgba(0, 0, 0, 0.45));
  }
  .rs-drawer__card-label {
    font-size: var(--text-xs, 0.75rem);
    color: var(--text-secondary, rgba(0, 0, 0, 0.65));
    text-align: center;
    line-height: 1.3;
    word-break: break-word;
    max-width: 100%;
  }
  .rs-drawer__card-check {
    position: absolute;
    top: var(--space-xs, 4px);
    right: var(--space-xs, 4px);
    font-size: var(--icon-md, 1.125rem);
    color: var(--color-primary, #6366f1);
  }
  .rs-drawer__row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-sm, 8px) var(--space-md, 12px);
    border: 1px solid var(--border-color, #e5e7eb);
    border-radius: var(--radius-md, 6px);
    background: var(--surface-primary, #fff);
    cursor: pointer;
    transition: border-color var(--transition-fast, 0.15s), background var(--transition-fast, 0.15s);
    font-size: var(--text-sm, 0.875rem);
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
  }
  .rs-drawer__row:hover { border-color: var(--color-primary, #6366f1); }
  .rs-drawer__row--selected {
    border-color: var(--color-primary, #6366f1);
    background: var(--color-primary-bg, rgba(99, 102, 241, 0.08));
  }
  .rs-drawer__row-check {
    font-size: var(--icon-md, 1.125rem);
    color: var(--color-primary, #6366f1);
  }
`;

class CrapRelationshipSearch extends HTMLElement {
  /** @type {boolean} */
  static _stylesInjected = false;

  /** Push the module-level sheet onto `document.adoptedStyleSheets` once per page. */
  static _injectStyles() {
    if (CrapRelationshipSearch._stylesInjected) return;
    CrapRelationshipSearch._stylesInjected = true;
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  }

  constructor() {
    super();

    // ── Configuration (filled by `_readConfig` at connect) ────────
    /** @type {string} */ this._collection = '';
    /** @type {string} */ this._fieldName = '';
    /** @type {string[]} */ this._collections = [];
    /** @type {boolean} */ this._hasMany = false;
    /** @type {boolean} */ this._required = false;
    /** @type {boolean} */ this._readonly = false;
    /** @type {boolean} */ this._polymorphic = false;
    /** @type {boolean} */ this._isUpload = false;
    /** @type {string} */ this._pickerMode = '';
    /** @type {boolean} */ this._hasError = false;

    // ── State ──────────────────────────────────────────────────────
    /** @type {boolean} */ this._initialized = false;
    /** @type {Item[]} */ this._selected = [];
    /** @type {Item[]} */ this._results = [];
    /** @type {number} */ this._activeIndex = -1;
    /** @type {boolean} */ this._suppressFocus = false;
    /** @type {ReturnType<typeof setTimeout>|null} */ this._debounceTimer = null;
    /** @type {AbortController|null} */ this._searchAbort = null;
    /** @type {MutationObserver|null} */ this._observer = null;

    // ── DOM refs (filled by `_buildDOM`) ───────────────────────────
    /** @type {HTMLDivElement|null} */ this._hiddenContainer = null;
    /** @type {HTMLDivElement|null} */ this._inputWrapper = null;
    /** @type {HTMLDivElement|null} */ this._tagsContainer = null;
    /** @type {HTMLElement|null} */ this._chipsContainer = null;
    /** @type {HTMLInputElement|null} */ this._input = null;
    /** @type {HTMLDivElement|null} */ this._dropdown = null;
    /** @type {HTMLButtonElement|null} */ this._clearBtn = null;
  }

  connectedCallback() {
    if (this._initialized) return;
    this._initialized = true;
    CrapRelationshipSearch._injectStyles();

    this._readConfig();
    this._buildDOM();
    this._wireInputEvents();
    this._setupClearButton();
    this._setupDrawerPicker();
    this._setupInlineCreate();
    this._setupPickEvent();
    this._setupPillRemoval();

    this._syncHiddenInputs();
    if (this._hasMany) this._renderChips();
    else this._renderHasOneDisplay();
  }

  disconnectedCallback() {
    if (this._observer) {
      this._observer.disconnect();
      this._observer = null;
    }
    if (this._searchAbort) {
      this._searchAbort.abort();
      this._searchAbort = null;
    }
    if (this._debounceTimer != null) {
      clearTimeout(this._debounceTimer);
      this._debounceTimer = null;
    }
    // Do NOT reset _initialized — DOM, listeners, and selection state survive
    // DOM moves. Resetting would cause full DOM rebuild and state loss.
  }

  /* ── Config + DOM construction ──────────────────────────────── */

  _readConfig() {
    this._collection = this.getAttribute('collection') || '';
    this._fieldName = this.getAttribute('field-name') || '';
    this._hasMany = this.hasAttribute('has-many');
    this._required = this.hasAttribute('required');
    this._readonly = this.hasAttribute('readonly');
    this._polymorphic = this.hasAttribute('polymorphic');
    this._isUpload = (this.getAttribute('field-type') || 'relationship') === 'upload';
    this._pickerMode = this.getAttribute('picker') || '';
    this._hasError = this.hasAttribute('data-error');

    this._collections = [this._collection];
    if (this._polymorphic) {
      try {
        const raw = JSON.parse(this.getAttribute('collections') || '[]');
        if (Array.isArray(raw) && raw.length > 0) this._collections = raw;
      } catch {
        /* fallback to single */
      }
    }

    try {
      const raw = JSON.parse(this.getAttribute('selected') || '[]');
      this._selected = Array.isArray(raw) ? raw : [];
    } catch {
      this._selected = [];
    }
  }

  _buildDOM() {
    clear(this);
    this._hiddenContainer = h('div', { class: 'relationship-search__hidden' });
    this._input = this._buildInput();
    this._inputWrapper = this._hasMany
      ? this._buildTagsWrapper(this._input)
      : h('div', { class: 'relationship-search__input-wrapper' }, this._input);
    this._dropdown = this._buildDropdown();
    this.append(this._hiddenContainer, this._inputWrapper, this._dropdown);
  }

  _buildInput() {
    const errorClass = this._hasError ? 'form__input--error' : null;
    return h('input', {
      type: 'text',
      class: this._hasMany
        ? 'relationship-search__tags-input'
        : ['relationship-search__input', errorClass],
      placeholder: this._hasMany ? t('search_to_add') : t('search'),
      id: `field-${this._fieldName}`,
      autocomplete: 'off',
      role: 'combobox',
      'aria-expanded': 'false',
      'aria-autocomplete': 'list',
      'aria-controls': `dropdown-${this._fieldName}`,
      disabled: this._readonly,
    });
  }

  /** @param {HTMLInputElement} input */
  _buildTagsWrapper(input) {
    // The chip cluster is its own atom (`<crap-pill-list>`); we own
    // canonical `this._selected` state and push it into the element's
    // `data-items` attribute. The element bubbles `crap:pill-removed`
    // events that we listen to via `_setupPillRemoval`.
    const pillList = document.createElement('crap-pill-list');
    if (this._polymorphic) pillList.setAttribute('data-polymorphic', '');
    if (this._readonly) pillList.setAttribute('data-readonly', '');
    this._chipsContainer = pillList;

    this._tagsContainer = h(
      'div',
      {
        class: ['relationship-search__tags', this._hasError && 'relationship-search__tags--error'],
        onClick: (e) => {
          if (e.target === this._tagsContainer) input.focus();
        },
      },
      pillList,
      input,
    );
    return h('div', { class: 'relationship-search__input-wrapper' }, this._tagsContainer);
  }

  _buildDropdown() {
    return h('div', {
      class: 'relationship-search__dropdown',
      id: `dropdown-${this._fieldName}`,
      role: 'listbox',
      hidden: true,
    });
  }

  /* ── State sync ─────────────────────────────────────────────── */

  _syncHiddenInputs() {
    if (!this._hiddenContainer) return;
    clear(this._hiddenContainer);
    this._hiddenContainer.appendChild(this._buildHiddenInput());
    this.dispatchEvent(new Event(EV_CHANGE, { bubbles: true }));
    this._updateViewLink();
  }

  _buildHiddenInput() {
    if (this._hasMany) {
      return h('input', {
        type: 'hidden',
        name: this._fieldName,
        value: this._selected.map((s) => s.id).join(','),
      });
    }

    const first = this._selected[0];
    /** @type {Record<string, any>} */
    const props = {
      type: 'hidden',
      name: this._fieldName,
      value: first?.id || '',
    };
    // Upload preview reads thumbnail metadata off the hidden input.
    if (this._isUpload && first) {
      if (first.thumbnail_url) props['data-thumbnail'] = first.thumbnail_url;
      if (first.filename) props['data-filename'] = first.filename;
      if (first.is_image) props['data-is-image'] = 'true';
    }
    return h('input', props);
  }

  /** Update the sibling view-link button based on current selection. */
  _updateViewLink() {
    const viewLink = /** @type {HTMLAnchorElement|null} */ (
      this.closest('.relationship-field')?.querySelector('.relationship-field__view-link')
    );
    if (!viewLink) return;

    const id = this._selected[0]?.id || '';
    if (!id) {
      viewLink.hidden = true;
      return;
    }
    const col = viewLink.getAttribute('data-collection') || this._collection;
    const href = `/admin/collections/${col}/${id}`;
    viewLink.setAttribute('href', href);
    viewLink.setAttribute('hx-get', href);
    viewLink.hidden = false;
    if (typeof htmx !== 'undefined') htmx.process(viewLink);
  }

  /* ── Rendering ──────────────────────────────────────────────── */

  /**
   * Push the canonical `_selected` state into the `<crap-pill-list>`
   * atom and toggle the parent's "has items" class. The atom emits
   * `crap:pill-removed` for remove clicks; we listen for that in
   * `_setupPillRemoval`.
   */
  _renderChips() {
    if (!this._chipsContainer) return;
    this._chipsContainer.setAttribute('data-items', JSON.stringify(this._selected));
    if (this._tagsContainer) {
      this._tagsContainer.classList.toggle(
        'relationship-search__tags--has-items',
        this._selected.length > 0,
      );
    }
  }

  /**
   * Wire the pill-list's remove event back into our `_selected` state.
   * Called once during `_buildDOM` after the element is in the tree.
   */
  _setupPillRemoval() {
    if (!this._chipsContainer || this._readonly) return;
    this._chipsContainer.addEventListener('crap:pill-removed', (e) => {
      const id = /** @type {CustomEvent<{ id: string }>} */ (e).detail.id;
      this._selected = this._selected.filter((s) => s.id !== id);
      this._renderChips();
      this._syncHiddenInputs();
    });
  }

  _renderHasOneDisplay() {
    if (this._hasMany || !this._input) return;
    const item = this._selected[0];
    if (item) {
      const prefix = this._polymorphic && item.collection ? `[${item.collection}] ` : '';
      this._input.value = prefix + item.label;
      this._input.dataset.selectedId = item.id;
    } else {
      this._input.value = '';
      this._input.dataset.selectedId = '';
    }
  }

  _renderDropdown() {
    if (!this._dropdown || !this._input) return;
    clear(this._dropdown);
    if (this._results.length === 0) {
      this._dropdown.append(
        h('div', {
          class: 'relationship-search__empty',
          text: t('no_results'),
        }),
      );
    } else {
      let currentGroup = null;
      this._results.forEach((item, idx) => {
        if (this._polymorphic && item.collection && item.collection !== currentGroup) {
          currentGroup = item.collection;
          this._dropdown?.append(
            h('div', {
              class: 'relationship-search__group-header',
              text: item.collection,
            }),
          );
        }
        this._dropdown?.append(this._buildOption(item, idx));
      });
    }
    this._dropdown.hidden = false;
    this._input.setAttribute('aria-expanded', 'true');
  }

  /**
   * @param {Item} item
   * @param {number} idx
   */
  _buildOption(item, idx) {
    const isActive = idx === this._activeIndex;
    const isSelected = this._selected.some((s) => s.id === item.id);
    return h('div', {
      class: [
        'relationship-search__option',
        isActive && 'relationship-search__option--active',
        isSelected && 'relationship-search__option--selected',
      ],
      role: 'option',
      text: item.label,
      onMousedown: (e) => {
        e.preventDefault();
        this._selectItem(item);
      },
    });
  }

  _closeDropdown() {
    if (this._searchAbort) this._searchAbort.abort();
    if (!this._dropdown || !this._input) return;
    this._dropdown.hidden = true;
    clear(this._dropdown);
    this._results = [];
    this._activeIndex = -1;
    this._input.setAttribute('aria-expanded', 'false');
  }

  /* ── Selection ──────────────────────────────────────────────── */

  /** @param {Item} item */
  _selectItem(item) {
    if (this._hasMany) {
      if (!this._selected.some((s) => s.id === item.id)) {
        this._selected.push(item);
        this._renderChips();
      }
      if (this._input) this._input.value = '';
    } else {
      this._selected = [item];
      this._renderHasOneDisplay();
    }
    this._syncHiddenInputs();
    this._closeDropdown();
  }

  /* ── Search ─────────────────────────────────────────────────── */

  /**
   * Search the configured collection(s) for `query` and render results.
   * Aborts any in-flight search so a stale response can't overwrite a
   * newer one.
   *
   * @param {string} query
   */
  async _doSearch(query) {
    if (this._searchAbort) this._searchAbort.abort();
    this._searchAbort = new AbortController();
    const signal = this._searchAbort.signal;

    try {
      this._results = this._polymorphic
        ? await this._searchPolymorphic(query, signal)
        : await this._searchSingle(this._collection, query, signal);
      if (signal.aborted) return;
      this._activeIndex = -1;
      this._renderDropdown();
    } catch {
      // Aborted or network error — leave results untouched.
    }
  }

  /**
   * @param {string} collection
   * @param {string} query
   * @param {AbortSignal} signal
   * @returns {Promise<Item[]>}
   */
  async _searchSingle(collection, query, signal) {
    const url = `/admin/api/search/${encodeURIComponent(collection)}?q=${encodeURIComponent(query)}&limit=20`;
    const resp = await fetch(url, { signal });
    if (!resp.ok) return [];
    return resp.json();
  }

  /**
   * Fan out across `_collections`, tag each result with its collection,
   * use composite `${col}/${id}` as the picker id, and group by collection.
   *
   * @param {string} query
   * @param {AbortSignal} signal
   * @returns {Promise<Item[]>}
   */
  async _searchPolymorphic(query, signal) {
    const buckets = await Promise.all(
      this._collections.map(async (col) => {
        try {
          const items = await this._searchSingle(col, query, signal);
          return items.map((item) => ({
            id: `${col}/${item.id}`,
            label: item.label,
            collection: col,
          }));
        } catch {
          return [];
        }
      }),
    );
    if (signal.aborted) return [];
    const flat = buckets.flat();
    flat.sort(
      (a, b) =>
        this._collections.indexOf(a.collection || '') -
        this._collections.indexOf(b.collection || ''),
    );
    return flat;
  }

  /* ── Input event wiring ─────────────────────────────────────── */

  _wireInputEvents() {
    const input = this._input;
    if (!input) return;
    input.addEventListener('input', () => this._onInput());
    input.addEventListener('focus', () => this._onFocus());
    input.addEventListener('blur', () => this._onBlur());
    input.addEventListener('keydown', (e) => this._onKeydown(e));
  }

  _onInput() {
    if (!this._input) return;
    const query = this._input.value.trim();
    if (this._debounceTimer != null) clearTimeout(this._debounceTimer);

    // Has-one: typing replaces a previous selection.
    if (!this._hasMany && this._input.dataset.selectedId) {
      this._selected = [];
      this._input.dataset.selectedId = '';
      this._syncHiddenInputs();
    }

    this._debounceTimer = setTimeout(() => this._doSearch(query), SEARCH_DEBOUNCE_MS);
  }

  _onFocus() {
    if (this._readonly || this._suppressFocus || !this._input) return;
    const query = !this._hasMany && this._input.dataset.selectedId ? '' : this._input.value.trim();
    this._doSearch(query);
  }

  _onBlur() {
    setTimeout(() => {
      this._closeDropdown();
      if (!this._hasMany) this._renderHasOneDisplay();
    }, BLUR_CLOSE_MS);
  }

  /** @param {KeyboardEvent} e */
  _onKeydown(e) {
    const count = this._results.length;
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        this._activeIndex = Math.min(this._activeIndex + 1, count - 1);
        this._renderDropdown();
        return;
      case 'ArrowUp':
        e.preventDefault();
        this._activeIndex = Math.max(this._activeIndex - 1, 0);
        this._renderDropdown();
        return;
      case 'Enter':
        e.preventDefault();
        if (count === 0) return;
        this._selectItem(this._results[this._activeIndex >= 0 ? this._activeIndex : 0]);
        return;
      case 'Escape':
        this._closeDropdown();
        if (!this._hasMany) this._renderHasOneDisplay();
        return;
      case 'Backspace':
        if (this._hasMany && this._input?.value === '' && this._selected.length > 0) {
          this._selected.pop();
          this._renderChips();
          this._syncHiddenInputs();
        }
        return;
    }
  }

  /* ── Clear button (has-one, optional) ───────────────────────── */

  _setupClearButton() {
    if (this._hasMany || this._required || this._readonly) return;
    if (!this._inputWrapper || !this._hiddenContainer) return;

    this._clearBtn = h('button', {
      type: 'button',
      class: 'relationship-search__clear',
      text: '×',
      title: t('clear_selection'),
      hidden: this._selected.length === 0,
      onClick: () => this._onClear(),
    });
    this._inputWrapper.appendChild(this._clearBtn);

    // Hidden-container changes ↔ clear button visibility. We could call
    // `_syncClearVisibility()` from every selection mutation, but a
    // MutationObserver is robust against future code paths that mutate
    // selection without going through `_syncHiddenInputs`.
    this._observer = new MutationObserver(() => this._syncClearVisibility());
    this._observer.observe(this._hiddenContainer, { childList: true, subtree: true });
  }

  _syncClearVisibility() {
    if (this._clearBtn) this._clearBtn.hidden = this._selected.length === 0;
  }

  _onClear() {
    this._selected = [];
    this._syncHiddenInputs();
    this._renderHasOneDisplay();
    this._syncClearVisibility();
  }

  /* ── External pick (drawer / picker) ────────────────────────── */

  _setupPickEvent() {
    this.addEventListener(EV_PICK, (e) => {
      this._suppressFocus = true;
      this._selectItem(/** @type {CustomEvent<Item>} */ (e).detail);
      setTimeout(() => {
        this._suppressFocus = false;
      }, SUPPRESS_FOCUS_MS);
    });
  }

  /* ── Drawer picker ──────────────────────────────────────────── */

  _setupDrawerPicker() {
    if (this._pickerMode !== 'drawer' || this._readonly) return;
    if (!this._inputWrapper?.parentNode) return;

    const browseBtn = h(
      'button',
      {
        type: 'button',
        class: 'relationship-search__browse',
        title: t('browse'),
        onClick: () => this._openDrawerPicker(),
      },
      h('span', { class: ['material-symbols-outlined', 'icon--md'], text: 'folder_open' }),
    );

    const row = h('div', { class: 'relationship-search__input-row' });
    this._inputWrapper.parentNode.insertBefore(row, this._inputWrapper);
    row.append(this._inputWrapper, browseBtn);
  }

  _openDrawerPicker() {
    const drawerEvt = new CustomEvent(EV_DRAWER_REQUEST, { detail: {} });
    document.dispatchEvent(drawerEvt);
    /** @type {{ open: (opts: { title: string }) => void, close: () => void, body: HTMLElement }|undefined} */
    const drawer = /** @type {any} */ (drawerEvt).detail.instance;
    if (!drawer) return;

    drawer.open({ title: this._isUpload ? t('browse_media') : t('browse') });

    const currentIds = this._readCurrentIds();
    const searchInput = this._buildDrawerSearch();
    const results = this._buildDrawerResults();
    const loadMore = this._buildDrawerLoadMore();

    drawer.body.append(searchInput, results, loadMore);

    this._wireDrawerPicker({ searchInput, results, loadMore, currentIds, drawer });
    searchInput.focus();
  }

  /** @returns {Set<string>} */
  _readCurrentIds() {
    const hidden = this._hiddenContainer?.querySelector('input[type="hidden"]');
    if (!(hidden instanceof HTMLInputElement) || !hidden.value) return new Set();
    return new Set(hidden.value.split(',').filter(Boolean));
  }

  _buildDrawerSearch() {
    return h('input', {
      class: 'rs-drawer__search',
      type: 'text',
      placeholder: t('search'),
      autocomplete: 'off',
      'aria-label': 'Search',
    });
  }

  _buildDrawerResults() {
    return h('div', {
      class: [
        'rs-drawer__results',
        this._isUpload ? 'rs-drawer__results--grid' : 'rs-drawer__results--list',
      ],
    });
  }

  _buildDrawerLoadMore() {
    return h('button', {
      type: 'button',
      class: 'rs-drawer__load-more',
      hidden: true,
      text: t('load_more'),
    });
  }

  /**
   * @param {object} ctx
   * @param {HTMLInputElement} ctx.searchInput
   * @param {HTMLDivElement} ctx.results
   * @param {HTMLButtonElement} ctx.loadMore
   * @param {Set<string>} ctx.currentIds
   * @param {{ close: () => void }} ctx.drawer
   */
  _wireDrawerPicker(ctx) {
    /** @type {ReturnType<typeof setTimeout>|null} */
    let debounceTimer = null;
    let offset = 0;
    /** @type {AbortController|null} */
    let fetchCtrl = null;

    const fetchPage = async (/** @type {string} */ query, /** @type {boolean} */ append) => {
      if (fetchCtrl) fetchCtrl.abort();
      fetchCtrl = new AbortController();
      if (!append) {
        clear(ctx.results);
        offset = 0;
      }
      try {
        const url =
          `/admin/api/search/${encodeURIComponent(this._collection)}` +
          `?q=${encodeURIComponent(query)}&limit=${DRAWER_PAGE_SIZE}&offset=${offset}`;
        const resp = await fetch(url, { signal: fetchCtrl.signal });
        if (!resp.ok) return;
        /** @type {Item[]} */
        const items = await resp.json();
        for (const item of items) {
          ctx.results.appendChild(
            this._isUpload
              ? this._buildUploadCard(item, ctx.currentIds, ctx.drawer)
              : this._buildListItem(item, ctx.currentIds, ctx.drawer),
          );
        }
        offset += items.length;
        ctx.loadMore.hidden = items.length < DRAWER_PAGE_SIZE;
      } catch {
        /* aborted or network error */
      }
    };

    ctx.searchInput.addEventListener('input', () => {
      if (debounceTimer != null) clearTimeout(debounceTimer);
      debounceTimer = setTimeout(
        () => fetchPage(ctx.searchInput.value.trim(), false),
        DRAWER_DEBOUNCE_MS,
      );
    });
    ctx.loadMore.addEventListener('click', () => fetchPage(ctx.searchInput.value.trim(), true));
    fetchPage('', false);
  }

  /**
   * @param {Item} item
   * @param {Set<string>} currentIds
   * @param {{ close: () => void }} drawer
   */
  _buildUploadCard(item, currentIds, drawer) {
    const isSelected = currentIds.has(item.id);
    const visual =
      item.thumbnail_url && item.is_image
        ? h('img', { class: 'rs-drawer__card-img', src: item.thumbnail_url, alt: item.label || '' })
        : h('span', {
            class: ['rs-drawer__card-icon', 'material-symbols-outlined'],
            text: 'description',
          });

    return h(
      'div',
      {
        class: ['rs-drawer__card', isSelected && 'rs-drawer__card--selected'],
        onClick: () => this._onDrawerPick(item, drawer),
      },
      visual,
      h('span', { class: 'rs-drawer__card-label', text: item.label || item.id }),
      isSelected &&
        h('span', {
          class: ['rs-drawer__card-check', 'material-symbols-outlined'],
          text: 'check_circle',
        }),
    );
  }

  /**
   * @param {Item} item
   * @param {Set<string>} currentIds
   * @param {{ close: () => void }} drawer
   */
  _buildListItem(item, currentIds, drawer) {
    const isSelected = currentIds.has(item.id);
    return h(
      'div',
      {
        class: ['rs-drawer__row', isSelected && 'rs-drawer__row--selected'],
        onClick: () => this._onDrawerPick(item, drawer),
      },
      h('span', { text: item.label || item.id }),
      isSelected &&
        h('span', { class: ['rs-drawer__row-check', 'material-symbols-outlined'], text: 'check' }),
    );
  }

  /**
   * @param {Item} item
   * @param {{ close: () => void }} drawer
   */
  _onDrawerPick(item, drawer) {
    this.dispatchEvent(new CustomEvent(EV_PICK, { detail: item }));
    if (!this._hasMany) drawer.close();
  }

  /* ── Inline create ──────────────────────────────────────────── */

  _setupInlineCreate() {
    if (this._readonly) return;
    const field = this.closest('.relationship-field') || this.closest('.upload-field');
    if (!field) return;

    field.addEventListener('click', (e) => {
      if (!(e.target instanceof Element)) return;
      const link = /** @type {HTMLElement|null} */ (e.target.closest('[data-inline-create]'));
      if (!link) return;
      e.preventDefault();
      this._openInlineCreatePanel(
        link.dataset.inlineCreate || '',
        link.dataset.inlineCreateLabel || '',
      );
    });
  }

  /**
   * @param {string} collection
   * @param {string} title
   */
  _openInlineCreatePanel(collection, title) {
    const evt = new CustomEvent(EV_CREATE_PANEL_REQUEST, { detail: {} });
    document.dispatchEvent(evt);
    /** @type {{ open: (opts: any) => void }|undefined} */
    const panel = /** @type {any} */ (evt).detail.instance;
    panel?.open({
      collection,
      title,
      onCreated: (item) => this._selectItem(item),
    });
  }
}

customElements.define('crap-relationship-search', CrapRelationshipSearch);
