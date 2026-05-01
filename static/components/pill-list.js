/**
 * Pill / chip list — `<crap-pill-list>`.
 *
 * Renders a horizontal chip cluster from a JSON-encoded item array.
 * Each chip optionally shows a polymorphic-collection prefix and an
 * `×` remove button that fires a bubbling `crap:pill-removed` event
 * with `{ id }` in detail.
 *
 * Light DOM, no shadow boundary — but the component injects its own
 * stylesheet onto `document.adoptedStyleSheets` once at first connect
 * (same pattern as `<crap-relationship-search>` / `<crap-create-panel>`),
 * so consumers don't need to ship `pill-list__*` rules in their own
 * CSS to make the chips look right. Stand-alone usability matters: a
 * `<crap-pill-list>` dropped into any has-many UI renders correctly
 * out of the box.
 *
 * @attr data-items     JSON-encoded `Item[]` array.
 * @attr data-readonly  Hides remove buttons when present.
 * @attr data-polymorphic  Shows `item.collection` prefix on each chip.
 *
 * @example
 *   const list = document.createElement('crap-pill-list');
 *   list.dataset.items = JSON.stringify([{ id: 'p1', label: 'Post 1' }]);
 *   list.addEventListener('crap:pill-removed', e => {
 *     console.log('removed', e.detail.id);
 *   });
 *   parent.appendChild(list);
 *
 *   // Update the items later:
 *   list.dataset.items = JSON.stringify(newItems);
 *
 * @module pill-list
 * @stability stable
 */

import { css } from './_internal/css.js';
import { clear, h } from './_internal/h.js';

/**
 * @typedef {{
 *   id: string,
 *   label: string,
 *   collection?: string,
 * }} Item
 */

const sheet = css`
  crap-pill-list { display: contents; }
  .pill-list__chip {
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
  .pill-list__chip-remove {
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
    transition:
      color var(--transition-fast),
      background var(--transition-fast);
  }
  .pill-list__chip-remove:hover {
    color: var(--color-danger);
    background: var(--color-danger-bg);
  }
  .pill-list__chip-collection {
    font-size: 0.7em;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-secondary);
    background: var(--surface-secondary);
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    margin-right: var(--space-xs);
  }
`;

export class CrapPillList extends HTMLElement {
  static observedAttributes = ['data-items', 'data-readonly', 'data-polymorphic'];

  /** @type {boolean} */
  static _stylesInjected = false;

  /** Push the module-level sheet onto `document.adoptedStyleSheets` once per page. */
  static _injectStyles() {
    if (CrapPillList._stylesInjected) return;
    CrapPillList._stylesInjected = true;
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  }

  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    CrapPillList._injectStyles();
    this._render();
  }

  attributeChangedCallback() {
    if (this._connected) this._render();
  }

  /**
   * Read the JSON-encoded `data-items` attribute. Returns `[]` for
   * missing or unparseable values (defensive — never throws).
   *
   * @returns {Item[]}
   */
  _readItems() {
    const raw = this.getAttribute('data-items');
    if (!raw) return [];
    try {
      const parsed = JSON.parse(raw);
      return Array.isArray(parsed) ? parsed : [];
    } catch {
      return [];
    }
  }

  _render() {
    const items = this._readItems();
    const readonly = this.hasAttribute('data-readonly');
    const polymorphic = this.hasAttribute('data-polymorphic');

    clear(this);
    for (const item of items) {
      this.appendChild(this._buildChip(item, readonly, polymorphic));
    }
  }

  /**
   * Build one chip. Class names use the `pill-list__*` namespace so the
   * component is self-contained — the stylesheet pushed by
   * `_injectStyles` is the only piece consumers need to render correctly.
   *
   * @param {Item} item
   * @param {boolean} readonly
   * @param {boolean} polymorphic
   */
  _buildChip(item, readonly, polymorphic) {
    return h(
      'span',
      { class: 'pill-list__chip' },
      polymorphic &&
        item.collection &&
        h('span', {
          class: 'pill-list__chip-collection',
          text: item.collection,
        }),
      item.label,
      !readonly &&
        h('button', {
          type: 'button',
          class: 'pill-list__chip-remove',
          text: '×',
          'aria-label': `Remove ${item.label}`,
          onClick: () => this._emitRemoved(item.id),
        }),
    );
  }

  /** @param {string} id */
  _emitRemoved(id) {
    this.dispatchEvent(
      new CustomEvent('crap:pill-removed', {
        bubbles: true,
        composed: true,
        detail: { id },
      }),
    );
  }
}

if (!customElements.get('crap-pill-list')) {
  customElements.define('crap-pill-list', CrapPillList);
}
