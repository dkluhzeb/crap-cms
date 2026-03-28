/**
 * Back-references lazy loader — `<crap-back-refs>`.
 *
 * Fetches and displays the list of documents that reference a target document.
 * Shows a "Show details" button that loads the data on demand.
 *
 * @module back-refs
 *
 * @example
 * <crap-back-refs slug="media" doc-id="m1">
 *   <button type="button" class="button button--ghost button--small">Show details</button>
 * </crap-back-refs>
 */

import { t } from './i18n.js';

class CrapBackRefs extends HTMLElement {
  connectedCallback() {
    const slug = this.getAttribute('slug');
    const docId = this.getAttribute('doc-id');

    if (!slug || !docId) return;

    /** @type {HTMLButtonElement | null} */
    const btn = this.querySelector('button');
    if (!btn) return;

    /** @type {boolean} */
    let loaded = false;

    btn.addEventListener('click', () => {
      if (loaded) return;
      loaded = true;

      btn.disabled = true;
      btn.textContent = '...';

      fetch(`/admin/collections/${slug}/${docId}/back-references`)
        .then((r) => r.json())
        .then((refs) => {
          btn.style.display = 'none';
          this._render(refs);
        })
        .catch(() => {
          btn.textContent = t('error') || 'Error';
          btn.disabled = false;
          loaded = false;
        });
    });
  }

  /**
   * Render the back-reference list into the component.
   * @param {Array<{owner_label: string, field_label: string, count: number}>} refs
   */
  _render(refs) {
    if (!refs.length) {
      const p = document.createElement('p');
      p.className = 'text--muted text--sm';
      p.textContent = t('no_details') || 'No details available.';
      this.appendChild(p);
      return;
    }

    const ul = document.createElement('ul');
    ul.className = 'text--sm';
    ul.style.margin = '0';
    ul.style.paddingLeft = 'var(--space-lg, 1rem)';

    for (const item of refs) {
      const li = document.createElement('li');
      li.style.marginBottom = 'var(--space-xs, 0.25rem)';

      const strong = document.createElement('strong');
      strong.textContent = item.owner_label;
      li.appendChild(strong);

      const docs = t('documents') || 'document(s)';
      li.appendChild(document.createTextNode(` \u2014 ${item.count} ${docs}`));

      if (item.field_label) {
        const span = document.createElement('span');
        span.className = 'text--muted';
        span.textContent = ` (${item.field_label})`;
        li.appendChild(span);
      }

      ul.appendChild(li);
    }

    this.appendChild(ul);
  }
}

customElements.define('crap-back-refs', CrapBackRefs);
