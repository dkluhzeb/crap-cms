/**
 * Relationship search component.
 *
 * Replaces static `<select>` elements with a debounced search input
 * and dropdown for relationship fields. Works for both has-one and
 * has-many relationships, including polymorphic (multi-collection).
 *
 * Activated by `data-relationship-search="true"` on a container element.
 * The container must have:
 *   - data-collection: target collection slug (primary / first)
 *   - data-field-name: form field name
 *   - data-has-many: "true" for multi-select (optional)
 *   - data-polymorphic: "true" for multi-collection (optional)
 *   - data-collections: JSON array of collection slugs (when polymorphic)
 *   - data-selected: JSON array of {id, label, collection?} for pre-selected items
 */

const DEBOUNCE_MS = 250;
const MIN_QUERY_LENGTH = 0;

/**
 * @param {HTMLElement} container
 */
function initSearchWidget(container) {
  if (container.hasAttribute('data-search-init')) return;
  container.setAttribute('data-search-init', 'true');

  const collection = container.dataset.collection;
  const fieldName = container.dataset.fieldName;
  const hasMany = container.dataset.hasMany === 'true';
  const required = container.dataset.required === 'true';
  const readonly = container.dataset.readonly === 'true';
  const errorClass = container.dataset.error ? ' form__input--error' : '';
  const polymorphic = container.dataset.polymorphic === 'true';

  /** @type {string[]} */
  let collections = [collection];
  if (polymorphic) {
    try {
      collections = JSON.parse(container.dataset.collections || '[]');
    } catch { /* fallback to single */ }
    if (collections.length === 0) collections = [collection];
  }

  /** @type {Array<{id: string, label: string, collection?: string}>} */
  let selected = [];
  try {
    selected = JSON.parse(container.dataset.selected || '[]');
  } catch { /* empty */ }

  // Build the DOM
  container.innerHTML = '';

  // Hidden input(s) for form submission
  const hiddenContainer = document.createElement('div');
  hiddenContainer.className = 'relationship-search__hidden';
  container.appendChild(hiddenContainer);

  // Selected items display (chips for has-many, single display for has-one)
  if (hasMany) {
    const chipsContainer = document.createElement('div');
    chipsContainer.className = 'relationship-search__chips';
    container.appendChild(chipsContainer);
  }

  // Search input
  const inputWrapper = document.createElement('div');
  inputWrapper.className = 'relationship-search__input-wrapper';
  const input = document.createElement('input');
  input.type = 'text';
  input.className = 'relationship-search__input' + errorClass;
  input.placeholder = hasMany ? 'Search to add...' : 'Search...';
  input.autocomplete = 'off';
  if (readonly) input.disabled = true;
  inputWrapper.appendChild(input);
  container.appendChild(inputWrapper);

  // Dropdown
  const dropdown = document.createElement('div');
  dropdown.className = 'relationship-search__dropdown';
  dropdown.style.display = 'none';
  container.appendChild(dropdown);

  let debounceTimer = null;
  let activeIndex = -1;
  let suppressFocus = false;
  /** @type {Array<{id: string, label: string, collection?: string}>} */
  let results = [];

  const isUpload = container.dataset.fieldType === 'upload';

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
    container.dispatchEvent(new Event('crap:change', { bubbles: true }));
  }

  function renderChips() {
    const chipsContainer = container.querySelector('.relationship-search__chips');
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
      empty.textContent = 'No results';
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
    dropdown.style.display = 'none';
    dropdown.innerHTML = '';
    results = [];
    activeIndex = -1;
  }

  /**
   * Search one or more collections and merge results.
   * @param {string} query
   */
  async function doSearch(query) {
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
    clearBtn.title = 'Clear selection';
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
  }

  // Listen for picks from external sources (e.g. drawer picker)
  container.addEventListener('crap:pick', (e) => {
    suppressFocus = true;
    selectItem(e.detail);
    setTimeout(() => { suppressFocus = false; }, 300);
  });

  // Initial render
  syncHiddenInputs();
  if (hasMany) renderChips();
  else renderHasOneDisplay();
}

function initAllSearchWidgets() {
  document.querySelectorAll('[data-relationship-search="true"]').forEach(initSearchWidget);
}

document.addEventListener('DOMContentLoaded', initAllSearchWidgets);
document.addEventListener('htmx:afterSettle', initAllSearchWidgets);
