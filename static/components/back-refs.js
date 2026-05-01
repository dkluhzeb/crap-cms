/**
 * Back-references lazy loader — `<crap-back-refs>`.
 *
 * Lazily fetches and displays the documents that reference a target
 * document. The trigger `<button>` is passed in as the default slot;
 * on click the component fetches the list and renders it into its
 * Shadow DOM, then hides the trigger.
 *
 * @module back-refs
 * @stability stable
 *
 * @example
 * <crap-back-refs slug="media" doc-id="m1">
 *   <button type="button" class="button button--ghost button--small">
 *     Show details
 *   </button>
 * </crap-back-refs>
 */

import { css } from './_internal/css.js';
import { h } from './_internal/h.js';
import { t } from './_internal/i18n.js';

/**
 * @typedef {Object} BackRef
 * @property {string} owner_label
 * @property {string} field_label
 * @property {number} count
 */

const sheet = css`
  :host { display: contents; }
  ul {
    list-style: disc;
    margin: 0;
    padding-left: var(--space-lg, 1rem);
    font-size: var(--text-sm);
    color: var(--text-primary);
  }
  li { margin-bottom: var(--space-xs, 0.25rem); }
  .muted { color: var(--text-tertiary); }
  .empty {
    margin: 0;
    color: var(--text-tertiary);
    font-size: var(--text-sm);
  }
`;

class CrapBackRefs extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._initialized = false;
    /** @type {boolean} */
    this._loaded = false;
    const root = this.attachShadow({ mode: 'open' });
    root.adoptedStyleSheets = [sheet];
    // Default slot exposes the externally-provided trigger button.
    root.appendChild(h('slot'));
  }

  connectedCallback() {
    if (this._initialized) return;
    this._initialized = true;

    const slug = this.getAttribute('slug');
    const docId = this.getAttribute('doc-id');
    if (!slug || !docId) return;

    const btn = /** @type {HTMLButtonElement|null} */ (this.querySelector('button'));
    if (!btn) return;

    btn.addEventListener('click', () => this._load(btn, slug, docId));
  }

  /**
   * Fetch back-references and replace the trigger with the rendered list.
   * Re-enables the trigger on failure so the user can retry.
   *
   * @param {HTMLButtonElement} btn
   * @param {string} slug
   * @param {string} docId
   */
  async _load(btn, slug, docId) {
    if (this._loaded) return;
    this._loaded = true;

    const originalLabel = btn.textContent;
    btn.disabled = true;
    btn.textContent = '…';

    try {
      const res = await fetch(`/admin/collections/${slug}/${docId}/back-references`);
      /** @type {BackRef[]} */
      const refs = await res.json();
      btn.hidden = true;
      this._render(refs);
    } catch {
      btn.textContent = originalLabel;
      btn.disabled = false;
      this._loaded = false;
    }
  }

  /**
   * Render the list, or an empty-state paragraph when there are no refs.
   * @param {BackRef[]} refs
   */
  _render(refs) {
    const root = /** @type {ShadowRoot} */ (this.shadowRoot);
    if (!refs.length) {
      root.appendChild(h('p', { class: 'empty', text: t('no_details') }));
      return;
    }

    const docs = t('documents');
    root.appendChild(
      h(
        'ul',
        null,
        refs.map((item) =>
          h(
            'li',
            null,
            h('strong', { text: item.owner_label }),
            ` — ${item.count} ${docs}`,
            item.field_label && h('span', { class: 'muted', text: ` (${item.field_label})` }),
          ),
        ),
      ),
    );
  }
}

customElements.define('crap-back-refs', CrapBackRefs);
