/**
 * Drawer picker — browse button + drawer UI for relationship/upload fields.
 *
 * For fields with `data-picker="drawer"`, injects a "Browse" button next to
 * the search input. Clicking opens a <crap-drawer> with a searchable grid
 * (uploads) or list (relationships).
 *
 * Selection dispatches a `crap:pick` CustomEvent on the container so the
 * existing relationship-search.js `selectItem` logic handles the rest.
 */

import { getDrawer } from './drawer.js';

const DEBOUNCE_MS = 300;
const PAGE_SIZE = 24;

/** Inline style for material icon spans (class doesn't apply inside Shadow DOM). */
const ICON_STYLE = "font-family: 'Material Symbols Outlined'; font-weight: normal; font-style: normal; font-feature-settings: 'liga'; -webkit-font-smoothing: antialiased;";

/**
 * @param {HTMLElement} container - A `.relationship-search[data-picker="drawer"]` element
 */
function initDrawerPicker(container) {
  if (container.hasAttribute('data-drawer-init')) return;
  container.setAttribute('data-drawer-init', 'true');

  const collection = container.dataset.collection;
  const fieldType = container.dataset.fieldType || 'relationship';
  const hasMany = container.dataset.hasMany === 'true';
  const readonly = container.dataset.readonly === 'true';
  if (readonly) return;

  const isUpload = fieldType === 'upload';

  // Find the input wrapper and wrap it + a browse button in a flex row
  const inputWrapper = container.querySelector('.relationship-search__input-wrapper');
  if (!inputWrapper) return;

  const row = document.createElement('div');
  row.className = 'relationship-search__input-row';
  inputWrapper.parentNode.insertBefore(row, inputWrapper);
  row.appendChild(inputWrapper);

  const browseBtn = document.createElement('button');
  browseBtn.type = 'button';
  browseBtn.className = 'relationship-search__browse';
  browseBtn.title = 'Browse';
  browseBtn.innerHTML = '<span style="' + ICON_STYLE + ' font-size: 18px;">folder_open</span>';
  row.appendChild(browseBtn);

  browseBtn.addEventListener('click', () => openPicker(container, collection, isUpload, hasMany));
}

/**
 * Open the drawer picker for a field.
 * @param {HTMLElement} container
 * @param {string} collection
 * @param {boolean} isUpload
 * @param {boolean} hasMany
 */
function openPicker(container, collection, isUpload, hasMany) {
  const drawer = getDrawer();
  const label = isUpload ? 'Browse Media' : 'Browse';
  drawer.open({ title: label });

  const body = drawer.body;

  // Get currently selected IDs
  const hiddenInput = container.querySelector('.relationship-search__hidden input[type="hidden"]');
  const currentIds = new Set();
  if (hiddenInput && hiddenInput.value) {
    hiddenInput.value.split(',').forEach((id) => { if (id) currentIds.add(id); });
  }

  // Search input
  const searchInput = document.createElement('input');
  searchInput.type = 'text';
  searchInput.placeholder = 'Search...';
  searchInput.autocomplete = 'off';
  Object.assign(searchInput.style, {
    width: '100%',
    boxSizing: 'border-box',
    padding: '8px 12px',
    border: '1px solid var(--border-color, #e5e7eb)',
    borderRadius: 'var(--radius-md, 6px)',
    fontSize: 'var(--text-sm, 0.875rem)',
    marginBottom: '12px',
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
      gap: '10px',
    });
  } else {
    Object.assign(results.style, {
      display: 'flex',
      flexDirection: 'column',
      gap: '4px',
    });
  }
  body.appendChild(results);

  // Load more button
  const loadMore = document.createElement('button');
  loadMore.type = 'button';
  loadMore.textContent = 'Load more';
  Object.assign(loadMore.style, {
    display: 'none',
    width: '100%',
    padding: '8px',
    marginTop: '12px',
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

    const limit = PAGE_SIZE;
    const url = `/admin/api/search/${encodeURIComponent(collection)}?q=${encodeURIComponent(query)}&limit=${limit}`;
    try {
      const resp = await fetch(url);
      if (!resp.ok) return;
      const items = await resp.json();

      items.forEach((item) => {
        const el = isUpload ? createUploadCard(item, currentIds, hasMany, container, drawer)
                            : createListItem(item, currentIds, hasMany, container, drawer);
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
    }, DEBOUNCE_MS);
  });

  loadMore.addEventListener('click', () => {
    loadMore.style.display = 'none';
  });

  // Initial load
  fetchResults('', false);
  searchInput.focus();
}

/**
 * Create a thumbnail card for upload results.
 * @param {Object} item
 * @param {Set<string>} currentIds
 * @param {boolean} hasMany
 * @param {HTMLElement} container
 * @param {CrapDrawer} drawer
 * @returns {HTMLElement}
 */
function createUploadCard(item, currentIds, hasMany, container, drawer) {
  const card = document.createElement('div');
  const isSelected = currentIds.has(item.id);
  Object.assign(card.style, {
    display: 'flex',
    flexDirection: 'column',
    alignItems: 'center',
    gap: '6px',
    padding: '10px',
    border: `2px solid ${isSelected ? 'var(--color-primary, #6366f1)' : 'var(--border-color, #e5e7eb)'}`,
    borderRadius: 'var(--radius-md, 6px)',
    background: isSelected ? 'var(--color-primary-bg, rgba(99, 102, 241, 0.08))' : 'var(--surface-primary, #fff)',
    cursor: 'pointer',
    transition: 'border-color 0.15s, background 0.15s',
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
      borderRadius: '4px',
    });
    card.appendChild(img);
  } else {
    const icon = document.createElement('span');
    icon.textContent = 'description';
    Object.assign(icon.style, {
      fontFamily: "'Material Symbols Outlined'",
      fontSize: '36px',
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
      top: '4px',
      right: '4px',
      fontSize: '18px',
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
 * @param {Object} item
 * @param {Set<string>} currentIds
 * @param {boolean} hasMany
 * @param {HTMLElement} container
 * @param {CrapDrawer} drawer
 * @returns {HTMLElement}
 */
function createListItem(item, currentIds, hasMany, container, drawer) {
  const row = document.createElement('div');
  const isSelected = currentIds.has(item.id);
  Object.assign(row.style, {
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'space-between',
    padding: '8px 12px',
    border: `1px solid ${isSelected ? 'var(--color-primary, #6366f1)' : 'var(--border-color, #e5e7eb)'}`,
    borderRadius: 'var(--radius-md, 6px)',
    background: isSelected ? 'var(--color-primary-bg, rgba(99, 102, 241, 0.08))' : 'var(--surface-primary, #fff)',
    cursor: 'pointer',
    transition: 'border-color 0.15s, background 0.15s',
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
      fontSize: '18px',
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

function initAllDrawerPickers() {
  document.querySelectorAll('[data-picker="drawer"]').forEach(initDrawerPicker);
}

document.addEventListener('DOMContentLoaded', initAllDrawerPickers);
document.addEventListener('htmx:afterSettle', initAllDrawerPickers);
