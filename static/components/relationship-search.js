/**
 * <crap-relationship-search> — Light DOM custom element for relationship
 * and upload fields.
 *
 * Replaces static `<select>` elements with a debounced search input
 * and dropdown. Works for has-one, has-many, and polymorphic relationships.
 * Absorbs view-link management and drawer-picker behavior.
 *
 * Attributes:
 *   collection     — target collection slug (primary / first)
 *   field-name     — form field name
 *   field-type     — "relationship" or "upload"
 *   has-many       — boolean attribute for multi-select
 *   polymorphic    — boolean attribute for multi-collection
 *   collections    — JSON array of collection slugs (when polymorphic)
 *   selected       — JSON array of {id, label, collection?} for pre-selected items
 *   picker         — "drawer" to enable the browse-drawer UI
 *   required       — boolean attribute
 *   readonly       — boolean attribute
 *   data-error     — boolean attribute for error styling
 */

import { getDrawer } from './drawer.js';
import { t } from './i18n.js';

const DEBOUNCE_MS = 250;
const MIN_QUERY_LENGTH = 0;
const DRAWER_DEBOUNCE_MS = 300;
const DRAWER_PAGE_SIZE = 24;

/** Inline style for material icon spans (class doesn't apply inside Shadow DOM). */
const ICON_STYLE = "font-family: 'Material Symbols Outlined'; font-weight: normal; font-style: normal; font-feature-settings: 'liga'; -webkit-font-smoothing: antialiased;";

class CrapRelationshipSearch extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._initialized = false;
  }

  static _injectStyles() {
    if (CrapRelationshipSearch._stylesInjected) return;
    CrapRelationshipSearch._stylesInjected = true;
    const s = document.createElement('style');
    s.textContent = CrapRelationshipSearch._styles();
    document.head.appendChild(s);
  }

  /** @type {boolean} */
  static _stylesInjected = false;

  connectedCallback() {
    if (this._initialized) return;
    this._initialized = true;
    CrapRelationshipSearch._injectStyles();
    /** @type {MutationObserver|null} */
    this._observer = null;

    const collection = this.getAttribute('collection') || '';
    const fieldName = this.getAttribute('field-name') || '';
    const hasMany = this.hasAttribute('has-many');
    const required = this.hasAttribute('required');
    const readonly = this.hasAttribute('readonly');
    const errorClass = this.hasAttribute('data-error') ? ' form__input--error' : '';
    const polymorphic = this.hasAttribute('polymorphic');
    const fieldType = this.getAttribute('field-type') || 'relationship';
    const pickerMode = this.getAttribute('picker') || '';
    const isUpload = fieldType === 'upload';

    /** @type {string[]} */
    let collections = [collection];
    if (polymorphic) {
      try {
        collections = JSON.parse(this.getAttribute('collections') || '[]');
      } catch { /* fallback to single */ }
      if (collections.length === 0) collections = [collection];
    }

    /** @type {Array<{id: string, label: string, collection?: string, thumbnail_url?: string, filename?: string, is_image?: boolean}>} */
    let selected = [];
    try {
      selected = JSON.parse(this.getAttribute('selected') || '[]') || [];
    } catch { /* empty */ }

    // Build the DOM
    this.innerHTML = '';

    // Hidden input(s) for form submission
    const hiddenContainer = document.createElement('div');
    hiddenContainer.className = 'relationship-search__hidden';
    this.appendChild(hiddenContainer);

    // Search input
    const inputWrapper = document.createElement('div');
    inputWrapper.className = 'relationship-search__input-wrapper';
    const input = document.createElement('input');
    input.type = 'text';
    input.id = 'field-' + fieldName;
    input.autocomplete = 'off';
    input.setAttribute('role', 'combobox');
    input.setAttribute('aria-expanded', 'false');
    input.setAttribute('aria-autocomplete', 'list');
    input.setAttribute('aria-controls', 'dropdown-' + fieldName);
    if (readonly) input.disabled = true;

    // Has-many: tag-style container with chips + inline input
    if (hasMany) {
      const tagsContainer = document.createElement('div');
      tagsContainer.className = 'relationship-search__tags' + (this.hasAttribute('data-error') ? ' relationship-search__tags--error' : '');

      const chipsContainer = document.createElement('div');
      chipsContainer.className = 'relationship-search__chips';
      tagsContainer.appendChild(chipsContainer);

      input.className = 'relationship-search__tags-input';
      input.placeholder = t('search_to_add');
      tagsContainer.appendChild(input);

      // Click on container focuses input
      tagsContainer.addEventListener('click', (e) => {
        if (/** @type {HTMLElement} */ (e.target) === tagsContainer) input.focus();
      });

      inputWrapper.appendChild(tagsContainer);
    } else {
      input.className = 'relationship-search__input' + errorClass;
      input.placeholder = t('search');
      inputWrapper.appendChild(input);
    }

    this.appendChild(inputWrapper);

    // Dropdown
    const dropdown = document.createElement('div');
    dropdown.className = 'relationship-search__dropdown';
    dropdown.id = 'dropdown-' + fieldName;
    dropdown.setAttribute('role', 'listbox');
    dropdown.style.display = 'none';
    this.appendChild(dropdown);

    let debounceTimer = null;
    let activeIndex = -1;
    let suppressFocus = false;
    let searchGen = 0;
    /** @type {Array<{id: string, label: string, collection?: string}>} */
    let results = [];

    function syncHiddenInputs() {
      hiddenContainer.innerHTML = '';
      if (hasMany) {
        const hidden = document.createElement('input');
        hidden.type = 'hidden';
        hidden.name = fieldName;
        hidden.value = selected.map((s) => s.id).join(',');
        hiddenContainer.appendChild(hidden);
      } else {
        const hidden = document.createElement('input');
        hidden.type = 'hidden';
        hidden.name = fieldName;
        hidden.value = selected.length > 0 ? selected[0].id : '';
        // Store upload metadata on the hidden input for preview updates
        if (isUpload && selected.length > 0) {
          const item = selected[0];
          if (item.thumbnail_url) hidden.setAttribute('data-thumbnail', item.thumbnail_url);
          if (item.filename) hidden.setAttribute('data-filename', item.filename);
          if (item.is_image) hidden.setAttribute('data-is-image', 'true');
        }
        hiddenContainer.appendChild(hidden);
      }
      // Notify parent (e.g. upload preview) that selection changed
      self.dispatchEvent(new Event('crap:change', { bubbles: true }));
      updateViewLink();
    }

    const self = this;

    /** Update the sibling view link (if any) based on current selection. */
    function updateViewLink() {
      const viewLink = /** @type {HTMLAnchorElement|null} */ (
        self.closest('.relationship-field')?.querySelector('.relationship-field__view-link')
      );
      if (!viewLink) return;

      const val = selected.length > 0 ? selected[0].id : '';
      const col = viewLink.getAttribute('data-collection') || collection;
      if (val) {
        const href = '/admin/collections/' + col + '/' + val;
        viewLink.setAttribute('href', href);
        viewLink.setAttribute('hx-get', href);
        viewLink.style.display = '';
      } else {
        viewLink.style.display = 'none';
      }
    }

    function renderChips() {
      const chipsContainer = self.querySelector('.relationship-search__chips');
      if (!chipsContainer) return;
      chipsContainer.innerHTML = '';
      selected.forEach((item) => {
        const chip = document.createElement('span');
        chip.className = 'relationship-search__chip';
        // Show collection tag for polymorphic
        if (polymorphic && item.collection) {
          const tag = document.createElement('span');
          tag.className = 'relationship-search__chip-collection';
          tag.textContent = item.collection;
          chip.appendChild(tag);
        }
        chip.appendChild(document.createTextNode(item.label));
        if (!readonly) {
          const removeBtn = document.createElement('button');
          removeBtn.type = 'button';
          removeBtn.className = 'relationship-search__chip-remove';
          removeBtn.textContent = '\u00d7';
          removeBtn.addEventListener('click', () => {
            selected = selected.filter((s) => s.id !== item.id);
            renderChips();
            syncHiddenInputs();
          });
          chip.appendChild(removeBtn);
        }
        chipsContainer.appendChild(chip);
      });

      // Toggle error class on tags container
      const tagsContainer = self.querySelector('.relationship-search__tags');
      if (tagsContainer) {
        tagsContainer.classList.toggle('relationship-search__tags--has-items', selected.length > 0);
      }
    }

    function renderHasOneDisplay() {
      if (hasMany) return;
      if (selected.length > 0) {
        const item = selected[0];
        const prefix = (polymorphic && item.collection) ? `[${item.collection}] ` : '';
        input.value = prefix + item.label;
        input.dataset.selectedId = item.id;
      } else {
        input.value = '';
        input.dataset.selectedId = '';
      }
    }

    function renderDropdown() {
      dropdown.innerHTML = '';
      if (results.length === 0) {
        const empty = document.createElement('div');
        empty.className = 'relationship-search__empty';
        empty.textContent = t('no_results');
        dropdown.appendChild(empty);
      } else {
        let currentGroup = null;
        results.forEach((item, idx) => {
          // Group header for polymorphic results
          if (polymorphic && item.collection && item.collection !== currentGroup) {
            currentGroup = item.collection;
            const header = document.createElement('div');
            header.className = 'relationship-search__group-header';
            header.textContent = item.collection;
            dropdown.appendChild(header);
          }

          const option = document.createElement('div');
          option.className = 'relationship-search__option';
          option.setAttribute('role', 'option');
          if (idx === activeIndex) option.classList.add('relationship-search__option--active');

          const isSelected = selected.some((s) => s.id === item.id);
          if (isSelected) option.classList.add('relationship-search__option--selected');

          option.textContent = item.label;
          option.addEventListener('mousedown', (e) => {
            e.preventDefault();
            selectItem(item);
          });
          dropdown.appendChild(option);
        });
      }
      dropdown.style.display = '';
      input.setAttribute('aria-expanded', 'true');
    }

    /** @param {{id: string, label: string, collection?: string}} item */
    function selectItem(item) {
      if (hasMany) {
        if (!selected.some((s) => s.id === item.id)) {
          selected.push(item);
          renderChips();
        }
        input.value = '';
      } else {
        selected = [item];
        renderHasOneDisplay();
      }
      syncHiddenInputs();
      closeDropdown();
    }

    function closeDropdown() {
      searchGen++;
      dropdown.style.display = 'none';
      dropdown.innerHTML = '';
      results = [];
      activeIndex = -1;
      input.setAttribute('aria-expanded', 'false');
    }

    /**
     * Search one or more collections and merge results.
     * @param {string} query
     */
    async function doSearch(query) {
      const gen = ++searchGen;
      if (polymorphic) {
        // Fan out to all target collections
        const promises = collections.map(async (col) => {
          const url = `/admin/api/search/${encodeURIComponent(col)}?q=${encodeURIComponent(query)}&limit=20`;
          try {
            const resp = await fetch(url);
            if (!resp.ok) return [];
            const items = await resp.json();
            // Tag each result with its collection and use composite id
            return items.map((item) => ({
              id: `${col}/${item.id}`,
              label: item.label,
              collection: col,
            }));
          } catch { return []; }
        });
        const allResults = await Promise.all(promises);
        if (gen !== searchGen) return;
        results = allResults.flat();
        // Sort: group by collection
        results.sort((a, b) => {
          const ca = collections.indexOf(a.collection);
          const cb = collections.indexOf(b.collection);
          return ca - cb;
        });
      } else {
        const url = `/admin/api/search/${encodeURIComponent(collection)}?q=${encodeURIComponent(query)}&limit=20`;
        try {
          const resp = await fetch(url);
          if (!resp.ok) return;
          if (gen !== searchGen) return;
          results = await resp.json();
        } catch { return; }
      }
      activeIndex = -1;
      renderDropdown();
    }

    // Input events
    input.addEventListener('input', () => {
      const query = input.value.trim();
      if (debounceTimer) clearTimeout(debounceTimer);

      // For has-one, clear selection when user types
      if (!hasMany && input.dataset.selectedId) {
        selected = [];
        input.dataset.selectedId = '';
        syncHiddenInputs();
      }

      debounceTimer = setTimeout(() => {
        if (query.length >= MIN_QUERY_LENGTH) {
          doSearch(query);
        } else {
          doSearch('');
        }
      }, DEBOUNCE_MS);
    });

    input.addEventListener('focus', () => {
      if (!readonly && !suppressFocus) {
        const query = (!hasMany && input.dataset.selectedId) ? '' : input.value.trim();
        doSearch(query);
      }
    });

    input.addEventListener('blur', () => {
      setTimeout(() => {
        closeDropdown();
        if (!hasMany) renderHasOneDisplay();
      }, 200);
    });

    input.addEventListener('keydown', (e) => {
      const optionCount = results.length;
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        activeIndex = Math.min(activeIndex + 1, optionCount - 1);
        renderDropdown();
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        activeIndex = Math.max(activeIndex - 1, 0);
        renderDropdown();
      } else if (e.key === 'Enter') {
        e.preventDefault();
        if (activeIndex >= 0 && activeIndex < optionCount) {
          selectItem(results[activeIndex]);
        } else if (optionCount > 0) {
          selectItem(results[0]);
        }
      } else if (e.key === 'Escape') {
        closeDropdown();
        if (!hasMany) renderHasOneDisplay();
      } else if (e.key === 'Backspace' && hasMany && input.value === '' && selected.length > 0) {
        selected.pop();
        renderChips();
        syncHiddenInputs();
      }
    });

    // Clear button for has-one (when not required)
    if (!hasMany && !required && !readonly) {
      const clearBtn = document.createElement('button');
      clearBtn.type = 'button';
      clearBtn.className = 'relationship-search__clear';
      clearBtn.textContent = '\u00d7';
      clearBtn.title = t('clear_selection');
      clearBtn.style.display = selected.length > 0 ? '' : 'none';
      clearBtn.addEventListener('click', () => {
        selected = [];
        syncHiddenInputs();
        renderHasOneDisplay();
        clearBtn.style.display = 'none';
      });
      inputWrapper.appendChild(clearBtn);

      const observer = new MutationObserver(() => {
        clearBtn.style.display = selected.length > 0 ? '' : 'none';
      });
      observer.observe(hiddenContainer, { childList: true, subtree: true });
      this._observer = observer;
    }

    // Listen for picks from external sources (e.g. drawer picker)
    this.addEventListener('crap:pick', (e) => {
      suppressFocus = true;
      selectItem(/** @type {CustomEvent} */ (e).detail);
      setTimeout(() => { suppressFocus = false; }, 300);
    });

    // Initial render
    syncHiddenInputs();
    if (hasMany) renderChips();
    else renderHasOneDisplay();

    // ── Drawer picker (when picker="drawer") ────────────────────
    if (pickerMode === 'drawer' && !readonly) {
      this._setupDrawerPicker(collection, isUpload, hasMany);
    }
  }

  disconnectedCallback() {
    if (this._observer) {
      this._observer.disconnect();
      this._observer = null;
    }
    // Do NOT reset _initialized — DOM, listeners, and selection state survive
    // DOM moves. Resetting would cause full DOM rebuild and state loss.
  }

  /**
   * Set up the drawer browse button and picker UI.
   *
   * @param {string} collection
   * @param {boolean} isUpload
   * @param {boolean} hasMany
   */
  _setupDrawerPicker(collection, isUpload, hasMany) {
    const inputWrapper = this.querySelector('.relationship-search__input-wrapper');
    if (!inputWrapper) return;

    const row = document.createElement('div');
    row.className = 'relationship-search__input-row';
    inputWrapper.parentNode.insertBefore(row, inputWrapper);
    row.appendChild(inputWrapper);

    const browseBtn = document.createElement('button');
    browseBtn.type = 'button';
    browseBtn.className = 'relationship-search__browse';
    browseBtn.title = t('browse');
    browseBtn.innerHTML = '<span style="' + ICON_STYLE + ' font-size: var(--icon-md, 1.125rem);">folder_open</span>';
    row.appendChild(browseBtn);

    browseBtn.addEventListener('click', () => {
      this._openDrawerPicker(collection, isUpload, hasMany);
    });
  }

  /**
   * Open the drawer picker for browsing.
   *
   * @param {string} collection
   * @param {boolean} isUpload
   * @param {boolean} hasMany
   */
  _openDrawerPicker(collection, isUpload, hasMany) {
    const drawer = getDrawer();
    const label = isUpload ? t('browse_media') : t('browse');
    drawer.open({ title: label });

    const body = drawer.body;
    const self = this;

    // Get currently selected IDs
    const hiddenInput = this.querySelector('.relationship-search__hidden input[type="hidden"]');
    const currentIds = new Set();
    if (hiddenInput && /** @type {HTMLInputElement} */ (hiddenInput).value) {
      /** @type {HTMLInputElement} */ (hiddenInput).value.split(',').forEach((id) => {
        if (id) currentIds.add(id);
      });
    }

    // Search input
    const searchInput = document.createElement('input');
    searchInput.type = 'text';
    searchInput.placeholder = t('search');
    searchInput.autocomplete = 'off';
    searchInput.setAttribute('aria-label', 'Search');
    Object.assign(searchInput.style, {
      width: '100%',
      boxSizing: 'border-box',
      padding: 'var(--space-sm, 8px) var(--space-md, 12px)',
      border: '1px solid var(--border-color, #e5e7eb)',
      borderRadius: 'var(--radius-md, 6px)',
      fontSize: 'var(--text-sm, 0.875rem)',
      marginBottom: 'var(--space-md, 12px)',
      background: 'var(--input-bg, #fff)',
      color: 'var(--text-primary, rgba(0, 0, 0, 0.88))',
    });
    body.appendChild(searchInput);

    // Results container
    const results = document.createElement('div');
    if (isUpload) {
      Object.assign(results.style, {
        display: 'grid',
        gridTemplateColumns: 'repeat(auto-fill, minmax(140px, 1fr))',
        gap: 'var(--space-md, 10px)',
      });
    } else {
      Object.assign(results.style, {
        display: 'flex',
        flexDirection: 'column',
        gap: 'var(--space-xs, 4px)',
      });
    }
    body.appendChild(results);

    // Load more button
    const loadMore = document.createElement('button');
    loadMore.type = 'button';
    loadMore.textContent = t('load_more');
    Object.assign(loadMore.style, {
      display: 'none',
      width: '100%',
      padding: 'var(--space-sm, 8px)',
      marginTop: 'var(--space-md, 12px)',
      border: '1px solid var(--border-color, #e5e7eb)',
      borderRadius: 'var(--radius-md, 6px)',
      background: 'transparent',
      cursor: 'pointer',
      fontSize: 'var(--text-sm, 0.875rem)',
      color: 'var(--text-secondary, rgba(0, 0, 0, 0.65))',
    });
    body.appendChild(loadMore);

    let debounceTimer = null;
    let currentOffset = 0;

    /**
     * Fetch results from the search API.
     * @param {string} query
     * @param {boolean} append
     */
    async function fetchResults(query, append) {
      if (!append) {
        results.innerHTML = '';
        currentOffset = 0;
      }

      const limit = DRAWER_PAGE_SIZE;
      const url = `/admin/api/search/${encodeURIComponent(collection)}?q=${encodeURIComponent(query)}&limit=${limit}&offset=${currentOffset}`;
      try {
        const resp = await fetch(url);
        if (!resp.ok) return;
        const items = await resp.json();

        items.forEach((item) => {
          const el = isUpload
            ? createUploadCard(item, currentIds, hasMany, self, drawer)
            : createListItem(item, currentIds, hasMany, self, drawer);
          results.appendChild(el);
        });

        currentOffset += items.length;
        loadMore.style.display = items.length >= limit ? '' : 'none';
      } catch { /* ignore */ }
    }

    searchInput.addEventListener('input', () => {
      if (debounceTimer) clearTimeout(debounceTimer);
      debounceTimer = setTimeout(() => {
        fetchResults(searchInput.value.trim(), false);
      }, DRAWER_DEBOUNCE_MS);
    });

    loadMore.addEventListener('click', () => {
      fetchResults(searchInput.value.trim(), true);
    });

    // Initial load
    fetchResults('', false);
    searchInput.focus();
  }

  static _styles() {
    return `
      crap-relationship-search {
        position: relative;
        display: block;
      }
      .relationship-search__input-wrapper {
        position: relative;
      }
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
      .relationship-search__clear:hover {
        color: var(--text-primary);
      }
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
      .relationship-search__tags--error {
        border-color: var(--color-danger);
      }
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
      .relationship-search__tags-input::placeholder {
        color: var(--text-tertiary);
      }
      .relationship-search__chips {
        display: contents;
      }
      .relationship-search__chip {
        display: inline-flex;
        align-items: center;
        gap: var(--space-xs);
        padding: var(--space-xs) var(--space-sm);
        background: var(--color-primary-bg);
        border: 1px solid color-mix(in srgb, var(--color-primary) 20%, transparent);
        border-radius: var(--radius-md);
        font-size: var(--text-sm);
        font-weight: 500;
        color: var(--text-primary);
        line-height: 1.4;
        white-space: nowrap;
      }
      .relationship-search__chip-remove {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        background: none;
        border: none;
        cursor: pointer;
        color: var(--text-tertiary);
        font-size: var(--icon-sm);
        padding: 0;
        line-height: 1;
        margin-left: var(--space-2xs);
        border-radius: var(--radius-sm);
        transition: color var(--transition-fast), background var(--transition-fast);
      }
      .relationship-search__chip-remove:hover {
        color: var(--color-danger);
        background: var(--color-danger-bg);
      }
      .relationship-search__chip-collection {
        font-size: 0.7em;
        text-transform: uppercase;
        letter-spacing: 0.04em;
        color: var(--text-secondary);
        background: var(--surface-secondary);
        padding: 1px 5px;
        border-radius: var(--radius-sm);
        margin-right: var(--space-xs);
      }
      .relationship-search__group-header {
        font-size: var(--text-xs);
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: 0.04em;
        color: var(--text-secondary);
        padding: var(--space-xs2) var(--space-sm2) var(--space-xs);
        border-bottom: 1px solid var(--border-color);
      }
    `;
  }
}

/* ── Drawer picker helpers ─────────────────────────────────────── */

/**
 * Create a thumbnail card for upload results.
 *
 * @param {Object} item
 * @param {Set<string>} currentIds
 * @param {boolean} hasMany
 * @param {HTMLElement} container
 * @param {*} drawer
 * @returns {HTMLElement}
 */
function createUploadCard(item, currentIds, hasMany, container, drawer) {
  const card = document.createElement('div');
  const isSelected = currentIds.has(item.id);
  Object.assign(card.style, {
    display: 'flex',
    flexDirection: 'column',
    alignItems: 'center',
    gap: 'var(--space-sm, 6px)',
    padding: 'var(--space-md, 10px)',
    border: `2px solid ${isSelected ? 'var(--color-primary, #6366f1)' : 'var(--border-color, #e5e7eb)'}`,
    borderRadius: 'var(--radius-md, 6px)',
    background: isSelected ? 'var(--color-primary-bg, rgba(99, 102, 241, 0.08))' : 'var(--surface-primary, #fff)',
    cursor: 'pointer',
    transition: 'border-color var(--transition-fast, 0.15s), background var(--transition-fast, 0.15s)',
    minHeight: '100px',
    position: 'relative',
    overflow: 'hidden',
  });

  // Thumbnail or file icon
  if (item.thumbnail_url && item.is_image) {
    const img = document.createElement('img');
    img.src = item.thumbnail_url;
    img.alt = item.label || '';
    Object.assign(img.style, {
      width: '100%',
      height: '80px',
      objectFit: 'contain',
      borderRadius: 'var(--radius-sm, 4px)',
    });
    card.appendChild(img);
  } else {
    const icon = document.createElement('span');
    icon.textContent = 'description';
    Object.assign(icon.style, {
      fontFamily: "'Material Symbols Outlined'",
      fontSize: 'var(--control-lg, 2.25rem)',
      color: 'var(--text-tertiary, rgba(0, 0, 0, 0.45))',
    });
    card.appendChild(icon);
  }

  // Label
  const label = document.createElement('span');
  label.textContent = item.label || item.id;
  Object.assign(label.style, {
    fontSize: 'var(--text-xs, 0.75rem)',
    color: 'var(--text-secondary, rgba(0, 0, 0, 0.65))',
    textAlign: 'center',
    lineHeight: '1.3',
    wordBreak: 'break-word',
    maxWidth: '100%',
  });
  card.appendChild(label);

  // Selected indicator
  if (isSelected) {
    const check = document.createElement('span');
    check.textContent = 'check_circle';
    Object.assign(check.style, {
      fontFamily: "'Material Symbols Outlined'",
      position: 'absolute',
      top: 'var(--space-xs, 4px)',
      right: 'var(--space-xs, 4px)',
      fontSize: 'var(--icon-md, 1.125rem)',
      color: 'var(--color-primary, #6366f1)',
    });
    card.appendChild(check);
  }

  card.addEventListener('click', () => {
    container.dispatchEvent(new CustomEvent('crap:pick', { detail: item }));
    if (!hasMany) drawer.close();
  });

  card.addEventListener('mouseenter', () => {
    if (!isSelected) card.style.borderColor = 'var(--color-primary, #6366f1)';
  });
  card.addEventListener('mouseleave', () => {
    if (!isSelected) card.style.borderColor = 'var(--border-color, #e5e7eb)';
  });

  return card;
}

/**
 * Create a list item for relationship results.
 *
 * @param {Object} item
 * @param {Set<string>} currentIds
 * @param {boolean} hasMany
 * @param {HTMLElement} container
 * @param {*} drawer
 * @returns {HTMLElement}
 */
function createListItem(item, currentIds, hasMany, container, drawer) {
  const row = document.createElement('div');
  const isSelected = currentIds.has(item.id);
  Object.assign(row.style, {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    padding: 'var(--space-sm, 8px) var(--space-md, 12px)',
    border: `1px solid ${isSelected ? 'var(--color-primary, #6366f1)' : 'var(--border-color, #e5e7eb)'}`,
    borderRadius: 'var(--radius-md, 6px)',
    background: isSelected ? 'var(--color-primary-bg, rgba(99, 102, 241, 0.08))' : 'var(--surface-primary, #fff)',
    cursor: 'pointer',
    transition: 'border-color var(--transition-fast, 0.15s), background var(--transition-fast, 0.15s)',
    fontSize: 'var(--text-sm, 0.875rem)',
    color: 'var(--text-primary, rgba(0, 0, 0, 0.88))',
  });

  const label = document.createElement('span');
  label.textContent = item.label || item.id;
  row.appendChild(label);

  if (isSelected) {
    const check = document.createElement('span');
    check.textContent = 'check';
    Object.assign(check.style, {
      fontFamily: "'Material Symbols Outlined'",
      fontSize: 'var(--icon-md, 1.125rem)',
      color: 'var(--color-primary, #6366f1)',
    });
    row.appendChild(check);
  }

  row.addEventListener('click', () => {
    container.dispatchEvent(new CustomEvent('crap:pick', { detail: item }));
    if (!hasMany) drawer.close();
  });

  row.addEventListener('mouseenter', () => {
    if (!isSelected) row.style.borderColor = 'var(--color-primary, #6366f1)';
  });
  row.addEventListener('mouseleave', () => {
    if (!isSelected) row.style.borderColor = 'var(--border-color, #e5e7eb)';
  });

  return row;
}

customElements.define('crap-relationship-search', CrapRelationshipSearch);
