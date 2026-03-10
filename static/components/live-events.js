/**
 * Live event stream (SSE) — `<crap-live-events>`.
 *
 * Connects to /admin/events and shows toast notifications when documents
 * are created, updated, or deleted. Auto-reconnects on connection loss.
 * Detects concurrent edits and shows stale content warnings.
 *
 * @module live-events
 */

import { t } from './i18n.js';

class CrapLiveEvents extends HTMLElement {
  constructor() {
    super();
    /** @type {EventSource|null} */
    this._source = null;
    /** @type {number} */
    this._lastSaveTime = 0;
  }

  connectedCallback() {
    if (typeof EventSource === 'undefined') return;
    if (!document.querySelector('[data-admin-layout]')) return;

    this._onBeforeRequest = /** @param {CustomEvent} e */ (e) => {
      if (e.detail.requestConfig.verb !== 'get') {
        this._lastSaveTime = Date.now();
      }
    };
    document.addEventListener('htmx:beforeRequest', this._onBeforeRequest);

    this._connect();
  }

  disconnectedCallback() {
    if (this._source) {
      this._source.close();
      this._source = null;
    }
    if (this._onBeforeRequest) {
      document.removeEventListener('htmx:beforeRequest', this._onBeforeRequest);
    }
  }

  _connect() {
    this._source = new EventSource('/admin/events');
    const SAVE_GRACE_MS = 5000;

    this._source.addEventListener('mutation', /** @param {MessageEvent} e */ (e) => {
      try {
        const event = JSON.parse(e.data);
        const op = event.operation;
        const collection = event.collection;

        // Check if this event targets the document currently being edited
        const form = document.getElementById('edit-form');
        if (form) {
          const docId = form.dataset.documentId;
          const globalSlug = form.dataset.globalSlug;
          const currentUserId = form.dataset.currentUserId;
          const collectionSlug = form.dataset.collectionSlug;

          const isCurrentDoc =
            (docId && event.document_id === docId && event.collection === collectionSlug) ||
            (globalSlug && event.target === 'global' && event.collection === globalSlug);

          const isSelf = currentUserId && event.edited_by && event.edited_by.id === currentUserId;
          const isOwnSave = isSelf && (Date.now() - this._lastSaveTime < SAVE_GRACE_MS);
          if (isCurrentDoc && (op === 'delete' || op === 'update') && !isOwnSave) {
            this._showStaleWarning(op === 'delete' ? 'deleted' : 'updated', event.edited_by || null);
            return;
          }
        }

        /** @type {Record<string, string>} */
        const opLabels = { create: t('op_created'), update: t('op_updated'), delete: t('op_deleted') };
        const msg = `${collection} ${opLabels[op] || op}`;
        if (window.CrapToast) window.CrapToast.show(msg, 'info');
      } catch {
        // Ignore parse errors
      }
    });

    this._source.onerror = () => {
      if (this._source && this._source.readyState === EventSource.CLOSED) {
        this._source = null;
        setTimeout(() => this._connect(), 5000);
      }
    };
  }

  /**
   * @param {'updated'|'deleted'} action
   * @param {{ id: string, email: string }|null} editedBy
   */
  _showStaleWarning(action, editedBy) {
    const form = document.getElementById('edit-form');
    if (!form) return;

    const isDeleted = action === 'deleted';
    const who = editedBy ? editedBy.email : t('another_user');

    let banner = document.getElementById('stale-warning');
    if (!banner) {
      banner = document.createElement('div');
      banner.id = 'stale-warning';
      banner.className = 'stale-warning';
      form.parentNode.insertBefore(banner, form);
    }

    const message = isDeleted
      ? t('stale_deleted', { who })
      : t('stale_updated', { who });

    banner.innerHTML = `
      <span class="stale-warning__icon">&#9888;</span>
      <span class="stale-warning__text">${message}</span>
      <span class="stale-warning__actions">
        ${isDeleted ? '' : `<button type="button" class="stale-warning__reload button button--ghost button--small">${t('reload')}</button>`}
        <button type="button" class="stale-warning__dismiss">&times;</button>
      </span>
    `;

    const reloadBtn = banner.querySelector('.stale-warning__reload');
    if (reloadBtn) reloadBtn.onclick = () => location.reload();

    const dismissBtn = banner.querySelector('.stale-warning__dismiss');
    if (dismissBtn) dismissBtn.onclick = () => banner.remove();

    if (isDeleted) {
      form.querySelectorAll('input, select, textarea, button[type="submit"]').forEach(
        /** @param {HTMLInputElement} el */ (el) => { el.disabled = true; }
      );
    }
  }
}

customElements.define('crap-live-events', CrapLiveEvents);
