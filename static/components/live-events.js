/**
 * Live event stream — `<crap-live-events>`.
 *
 * Subscribes to `/admin/events` (server-sent events) and:
 *   - toasts on `mutation` events for collections + globals;
 *   - shows a stale-content banner above `#edit-form` when the document
 *     the user is editing was modified or deleted by someone else.
 *
 * Auto-reconnects on connection loss.
 *
 * @module live-events
 */

import { css } from './css.js';
import { h } from './h.js';
import { t } from './i18n.js';
import { getHttpVerb } from './util/htmx.js';
import { toast } from './util/toast.js';

/** Within this window after a save, our own update events are filtered out. */
const SAVE_GRACE_MS = 5000;

/** Reconnect delay after the SSE connection drops. */
const RECONNECT_DELAY_MS = 5000;

/**
 * Banner styles. Lives on `document.adoptedStyleSheets` (CSP-exempt
 * constructable stylesheet) — we used to inject a `<style>` block into
 * `<head>`, which the page's `style-src 'self'` CSP rejects.
 */
const sheet = css`
  .stale-warning {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    background: var(--color-warning-bg, #fef3c7);
    border: 1px solid var(--color-warning, #f59e0b);
    border-radius: var(--radius-md);
    margin-bottom: var(--space-md);
    font-size: var(--text-sm);
    color: var(--text-primary);
  }
  .stale-warning__icon { font-size: var(--text-lg); flex-shrink: 0; }
  .stale-warning__text { flex: 1; }
  .stale-warning__actions {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    flex-shrink: 0;
  }
  .stale-warning__reload { white-space: nowrap; }
  .stale-warning__dismiss {
    all: unset;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: var(--space-xl);
    height: var(--space-xl);
    border-radius: var(--radius-sm);
    cursor: pointer;
    font-size: var(--icon-md);
    color: var(--text-secondary);
  }
  .stale-warning__dismiss:hover {
    background: rgba(0, 0, 0, 0.1);
    color: var(--text-primary);
  }
  @media (max-width: 768px) {
    .stale-warning { flex-wrap: wrap; }
  }
`;

/**
 * @typedef {{ id: string, email: string }} Editor
 *
 * @typedef {{
 *   operation: 'create' | 'update' | 'delete',
 *   collection: string,
 *   document_id?: string,
 *   target?: 'global' | 'collection',
 *   edited_by?: Editor,
 * }} MutationEvent
 *
 * @typedef {{
 *   docId: string,
 *   collectionSlug: string,
 *   globalSlug: string,
 *   currentUserId: string,
 * }} EditFormCtx
 */

/**
 * Read the relevant identifiers off `#edit-form` so we can decide
 * whether an SSE event targets the document the user is editing.
 *
 * @returns {EditFormCtx|null}
 */
function readEditFormCtx() {
  const form = document.getElementById('edit-form');
  if (!form) return null;
  return {
    docId: form.dataset.documentId || '',
    collectionSlug: form.dataset.collectionSlug || '',
    globalSlug: form.dataset.globalSlug || '',
    currentUserId: form.dataset.currentUserId || '',
  };
}

/**
 * @param {MutationEvent} event
 * @param {EditFormCtx} ctx
 */
function eventTargetsCurrentDoc(event, ctx) {
  if (ctx.docId && event.document_id === ctx.docId && event.collection === ctx.collectionSlug) {
    return true;
  }
  if (ctx.globalSlug && event.target === 'global' && event.collection === ctx.globalSlug) {
    return true;
  }
  return false;
}

class CrapLiveEvents extends HTMLElement {
  /** @type {boolean} */
  static _stylesInjected = false;

  /** Push the warning-banner sheet onto `document.adoptedStyleSheets` once per page. */
  static _injectStyles() {
    if (CrapLiveEvents._stylesInjected) return;
    CrapLiveEvents._stylesInjected = true;
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  }

  constructor() {
    super();
    /** @type {EventSource|null} */
    this._source = null;
    /** @type {number} */
    this._lastSaveTime = 0;
    /** @type {ReturnType<typeof setTimeout>|null} */
    this._reconnectTimer = null;
    /** @type {((e: Event) => void)|null} */
    this._onBeforeRequest = null;
  }

  connectedCallback() {
    if (typeof EventSource === 'undefined') return;
    if (!document.querySelector('[data-admin-layout]')) return;

    this._onBeforeRequest = (e) => {
      if (getHttpVerb(e) !== 'GET') this._lastSaveTime = Date.now();
    };
    document.addEventListener('htmx:beforeRequest', this._onBeforeRequest);

    this._connect();
  }

  disconnectedCallback() {
    if (this._source) {
      this._source.close();
      this._source = null;
    }
    if (this._reconnectTimer != null) {
      clearTimeout(this._reconnectTimer);
      this._reconnectTimer = null;
    }
    if (this._onBeforeRequest) {
      document.removeEventListener('htmx:beforeRequest', this._onBeforeRequest);
      this._onBeforeRequest = null;
    }
  }

  _connect() {
    if (this._reconnectTimer != null) {
      clearTimeout(this._reconnectTimer);
      this._reconnectTimer = null;
    }
    this._source = new EventSource('/admin/events');
    this._source.addEventListener('mutation', (e) => this._onMutation(e));
    this._source.onerror = () => this._onSseError();
  }

  /** @param {MessageEvent} e */
  _onMutation(e) {
    /** @type {MutationEvent|null} */
    let event;
    try {
      event = JSON.parse(e.data);
    } catch {
      return;
    }
    if (!event) return;

    const ctx = readEditFormCtx();
    if (ctx && this._isStaleEvent(event, ctx)) {
      this._showStaleWarning(
        event.operation === 'delete' ? 'deleted' : 'updated',
        event.edited_by || null,
      );
      return;
    }
    this._toastMutation(event);
  }

  /**
   * Whether the event should produce a stale-content warning instead of
   * a regular toast.
   *
   * @param {MutationEvent} event
   * @param {EditFormCtx} ctx
   */
  _isStaleEvent(event, ctx) {
    const op = event.operation;
    if (op !== 'update' && op !== 'delete') return false;
    if (!eventTargetsCurrentDoc(event, ctx)) return false;
    const isSelf = ctx.currentUserId && event.edited_by?.id === ctx.currentUserId;
    const isOwnSave = isSelf && Date.now() - this._lastSaveTime < SAVE_GRACE_MS;
    return !isOwnSave;
  }

  /** @param {MutationEvent} event */
  _toastMutation(event) {
    /** @type {Record<string, string>} */
    const labels = {
      create: t('op_created'),
      update: t('op_updated'),
      delete: t('op_deleted'),
    };
    toast({
      message: `${event.collection} ${labels[event.operation] || event.operation}`,
      type: 'info',
    });
  }

  _onSseError() {
    if (!this._source || this._source.readyState !== EventSource.CLOSED) return;
    this._source = null;
    this._reconnectTimer = setTimeout(() => this._connect(), RECONNECT_DELAY_MS);
  }

  /**
   * Render (or refresh) the stale-content banner above `#edit-form`.
   * Disables the form's inputs when the document was deleted.
   *
   * @param {'updated'|'deleted'} action
   * @param {Editor|null} editedBy
   */
  _showStaleWarning(action, editedBy) {
    CrapLiveEvents._injectStyles();
    const form = document.getElementById('edit-form');
    if (!form?.parentNode) return;

    const banner = this._ensureBanner(form);
    banner.replaceChildren(...this._buildBannerChildren(action, editedBy, banner));

    if (action === 'deleted') this._disableForm(form);
  }

  /**
   * Get the existing banner or insert a fresh one above `form`.
   * @param {HTMLElement} form
   */
  _ensureBanner(form) {
    const existing = document.getElementById('stale-warning');
    if (existing) return existing;
    const banner = h('div', { id: 'stale-warning', class: 'stale-warning' });
    form.parentNode?.insertBefore(banner, form);
    return banner;
  }

  /**
   * @param {'updated'|'deleted'} action
   * @param {Editor|null} editedBy
   * @param {HTMLElement} banner Reference passed so the dismiss button can self-remove.
   */
  _buildBannerChildren(action, editedBy, banner) {
    const isDeleted = action === 'deleted';
    const who = editedBy?.email || t('another_user');
    const message = isDeleted ? t('stale_deleted', { who }) : t('stale_updated', { who });

    return [
      h('span', { class: 'stale-warning__icon', text: '⚠' }),
      h('span', { class: 'stale-warning__text', text: message }),
      h(
        'span',
        { class: 'stale-warning__actions' },
        !isDeleted &&
          h('button', {
            type: 'button',
            class: ['stale-warning__reload', 'button', 'button--ghost', 'button--small'],
            text: t('reload'),
            onClick: () => location.reload(),
          }),
        h('button', {
          type: 'button',
          class: 'stale-warning__dismiss',
          text: '×',
          onClick: () => banner.remove(),
        }),
      ),
    ];
  }

  /** @param {HTMLElement} form */
  _disableForm(form) {
    for (const el of /** @type {NodeListOf<HTMLInputElement>} */ (
      form.querySelectorAll('input, select, textarea, button[type="submit"]')
    )) {
      el.disabled = true;
    }
  }
}

customElements.define('crap-live-events', CrapLiveEvents);
