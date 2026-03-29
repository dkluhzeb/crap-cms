/**
 * Inline Create Panel — `<crap-create-panel>`.
 *
 * Near-fullpage slideout dialog for creating related documents without
 * navigating away from the current edit page. Fetches the existing create
 * form, injects it into a light DOM `<dialog>`, intercepts submission,
 * and returns the created item to the caller.
 *
 * Usage:
 *   const panel = getCreatePanel(); // event-based discovery
 *   panel?.open({
 *     collection: 'posts',
 *     title: 'Create Post',
 *     onCreated: ({ id, label }) => { ... }
 *   });
 *
 * @module create-panel
 */

import { t } from './i18n.js';

class CrapCreatePanel extends HTMLElement {
  constructor() {
    super();
    /** @type {((item: {id: string, label: string}) => void)|null} */
    this._onCreated = null;
    /** @type {AbortController|null} */
    this._abortController = null;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    CrapCreatePanel._injectStyles();

    this._dialog = document.createElement('dialog');
    this._dialog.className = 'create-panel';
    this._dialog.innerHTML = `
      <div class="create-panel__header">
        <h2 class="create-panel__title"></h2>
        <button type="button" class="create-panel__close" aria-label="${t('close')}">&times;</button>
      </div>
      <div class="create-panel__body"></div>
    `;
    this.appendChild(this._dialog);

    this._dialog.querySelector('.create-panel__close').addEventListener('click', () => this.close());

    // Event-based discovery (same pattern as drawer/confirm-dialog)
    this._handleRequest = (e) => { e.detail.instance = this; };
    document.addEventListener('crap:create-panel-request', this._handleRequest);

    this._dialog.addEventListener('click', (e) => {
      if (e.target === this._dialog) this.close();
    });

    this._dialog.addEventListener('cancel', (e) => {
      e.preventDefault();
      this.close();
    });
  }

  /**
   * Open the create panel and load the create form for a collection.
   *
   * @param {{ collection: string, title: string, onCreated: (item: {id: string, label: string}) => void }} opts
   */
  async open(opts) {
    if (!this._dialog) return;

    this._onCreated = opts.onCreated || null;
    this._dialog.querySelector('.create-panel__title').textContent = opts.title || '';

    const body = this._dialog.querySelector('.create-panel__body');
    this._setBodyMessage(body, 'create-panel__loading', t('loading') || 'Loading...');
    this._dialog.showModal();

    // Abort any previous fetch
    if (this._abortController) this._abortController.abort();
    this._abortController = new AbortController();

    try {
      const resp = await fetch(`/admin/collections/${opts.collection}/create`, {
        signal: this._abortController.signal,
        headers: { 'X-Inline-Create': '1' },
      });

      if (!resp.ok) {
        this._setBodyMessage(body, 'create-panel__error', t('error') || 'Error');
        return;
      }

      const html = await resp.text();
      this._injectForm(body, html, opts.collection);
    } catch (e) {
      if (e.name !== 'AbortError') {
        this._setBodyMessage(body, 'create-panel__error', t('error') || 'Error');
      }
    }
  }

  /**
   * Parse the full create page response and extract the form into the panel.
   *
   * @param {HTMLElement} body
   * @param {string} html
   * @param {string} collection
   */
  _injectForm(body, html, collection) {
    const doc = new DOMParser().parseFromString(html, 'text/html');

    // Extract the edit form
    const form = doc.querySelector('#edit-form');
    if (!form) {
      body.innerHTML = '<p class="create-panel__error">' + (t('error') || 'Error') + '</p>';
      return;
    }

    // Remove dirty-form guard (we don't want unsaved-changes warnings inside the panel)
    form.querySelectorAll('crap-dirty-form').forEach((el) => {
      while (el.firstChild) el.parentNode.insertBefore(el.firstChild, el);
      el.remove();
    });

    // Remove scroll-restore (not needed in panel)
    form.querySelectorAll('crap-scroll-restore').forEach((el) => {
      while (el.firstChild) el.parentNode.insertBefore(el.firstChild, el);
      el.remove();
    });

    // Strip HTMX attributes so HTMX doesn't intercept submission
    form.removeAttribute('hx-post');
    form.removeAttribute('hx-put');
    form.removeAttribute('hx-target');
    form.removeAttribute('hx-indicator');
    form.querySelectorAll('[hx-post],[hx-put],[hx-get],[hx-target]').forEach((el) => {
      el.removeAttribute('hx-post');
      el.removeAttribute('hx-put');
      el.removeAttribute('hx-get');
      el.removeAttribute('hx-target');
      el.removeAttribute('hx-push-url');
    });

    // Flatten edit-layout for panel: stack content and sidebar vertically
    const editLayout = form.querySelector('.edit-layout');
    if (editLayout) {
      editLayout.style.gridTemplateColumns = '1fr';
    }

    // Move sidebar above content (actions first)
    const sidebar = form.querySelector('.edit-layout__sidebar');
    const content = form.querySelector('.edit-layout__content');
    if (sidebar && content && editLayout) {
      editLayout.insertBefore(sidebar, content);
    }

    body.innerHTML = '';
    body.appendChild(form);

    // Intercept form submission
    form.addEventListener('submit', (e) => {
      e.preventDefault();
      this._submitForm(form, body, collection);
    });
  }

  /**
   * Submit the form via fetch and handle the response.
   *
   * @param {HTMLFormElement} form
   * @param {HTMLElement} body
   * @param {string} collection
   */
  async _submitForm(form, body, collection) {
    const submitBtns = form.querySelectorAll('button[type="submit"], input[type="submit"]');
    const savedLabels = new Map();

    submitBtns.forEach((btn) => {
      btn.disabled = true;
      savedLabels.set(btn, btn.textContent);
      btn.textContent = t('saving') || 'Saving...';
    });

    try {
      const formData = new FormData(form);
      const action = form.getAttribute('action') || `/admin/collections/${collection}`;
      const method = form.getAttribute('method') || 'POST';

      // Ensure CSRF token is in form data
      const csrfCookie = document.cookie.match(/(?:^|; )crap_csrf=([^;]*)/);
      if (csrfCookie && !formData.has('_csrf')) {
        formData.set('_csrf', csrfCookie[1]);
      }

      const headers = { 'X-Inline-Create': '1' };
      const csrfMatch = document.cookie.match(/(?:^|; )crap_csrf=([^;]*)/);
      if (csrfMatch) headers['X-CSRF-Token'] = csrfMatch[1];

      const resp = await fetch(action, {
        method: method.toUpperCase(),
        body: formData,
        headers,
        redirect: 'manual',
      });

      const createdId = resp.headers.get('X-Created-Id');
      const createdLabel = resp.headers.get('X-Created-Label');

      if (createdId) {
        // Success — item was created
        if (this._onCreated) {
          this._onCreated({ id: createdId, label: createdLabel || createdId });
        }

        document.dispatchEvent(new CustomEvent('crap:toast', { detail: { message: createdLabel || createdId, type: 'success' } }));

        this.close();
        return;
      }

      // Validation error — re-render form in panel
      if (resp.ok || resp.status === 422) {
        const html = await resp.text();
        this._injectForm(body, html, collection);

        // Show toast if present
        const toastHeader = resp.headers.get('X-Crap-Toast');
        if (toastHeader) {
          try {
            const parsed = JSON.parse(toastHeader);
            document.dispatchEvent(new CustomEvent('crap:toast', { detail: { message: parsed.message, type: parsed.type || 'error' } }));
          } catch { /* ignore */ }
        }
      }
    } catch {
      document.dispatchEvent(new CustomEvent('crap:toast', { detail: { message: t('error') || 'Error', type: 'error' } }));
    } finally {
      submitBtns.forEach((btn) => {
        btn.disabled = false;
        btn.textContent = savedLabels.get(btn) || '';
      });
    }
  }

  /**
   * Set body to a single text message with a class.
   * @param {HTMLElement} body
   * @param {string} className
   * @param {string} message
   */
  _setBodyMessage(body, className, message) {
    body.innerHTML = '';
    const el = document.createElement('p');
    el.className = className;
    el.textContent = message;
    body.appendChild(el);
  }

  close() {
    if (!this._dialog) return;
    if (this._abortController) this._abortController.abort();
    this._abortController = null;
    this._dialog.close();
    this._dialog.querySelector('.create-panel__body').innerHTML = '';
    this._onCreated = null;
  }

  disconnectedCallback() {
    if (this._abortController) {
      this._abortController.abort();
      this._abortController = null;
    }
    document.removeEventListener('crap:create-panel-request', this._handleRequest);
  }

  static _stylesInjected = false;

  static _injectStyles() {
    if (CrapCreatePanel._stylesInjected) return;
    CrapCreatePanel._stylesInjected = true;

    const s = document.createElement('style');
    s.textContent = `
      .create-panel {
        position: fixed;
        inset: 0 0 0 auto;
        width: min(90vw, calc(100vw - var(--sidebar-width, 13rem)));
        max-width: none;
        max-height: none;
        height: 100vh;
        margin: 0;
        border: none;
        padding: 0;
        background: var(--bg-body, #f4f7fc);
        color: var(--text-primary, rgba(0, 0, 0, 0.88));
        box-shadow: var(--shadow-lg, 0 4px 16px rgba(0, 0, 0, 0.08));
        display: flex;
        flex-direction: column;
        animation: create-panel-in 0.25s ease forwards;
      }

      .create-panel:not([open]) {
        display: none;
      }

      .create-panel::backdrop {
        background: rgba(0, 0, 0, 0.4);
      }

      @keyframes create-panel-in {
        from { transform: translateX(100%); }
        to   { transform: translateX(0); }
      }

      .create-panel__header {
        display: flex;
        align-items: center;
        justify-content: space-between;
        padding: var(--space-md, 0.75rem) var(--space-lg, 1rem);
        border-bottom: 1px solid var(--border-color, rgba(0, 0, 0, 0.08));
        background: var(--bg-elevated, #fff);
        flex-shrink: 0;
      }

      .create-panel__title {
        font-size: var(--text-lg, 1rem);
        font-weight: 600;
        margin: 0;
      }

      .create-panel__close {
        all: unset;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: var(--control-md, 2rem);
        height: var(--control-md, 2rem);
        border-radius: var(--radius-sm, 4px);
        cursor: pointer;
        font-size: var(--icon-lg, 1.5rem);
        color: var(--text-secondary, rgba(0, 0, 0, 0.65));
      }

      .create-panel__close:hover {
        background: var(--bg-hover, rgba(0, 0, 0, 0.04));
        color: var(--text-primary, rgba(0, 0, 0, 0.88));
      }

      .create-panel__body {
        flex: 1;
        overflow-y: auto;
        padding: var(--space-lg, 1rem);
      }

      .create-panel__loading,
      .create-panel__error {
        text-align: center;
        padding: var(--space-2xl, 2rem);
        color: var(--text-tertiary, rgba(0, 0, 0, 0.45));
        font-size: var(--text-sm, 0.8125rem);
      }

      .create-panel__body .edit-layout {
        grid-template-columns: 1fr !important;
      }

      .create-panel__body .edit-layout__sidebar {
        position: static;
        order: -1;
      }

      @media (max-width: 1024px) {
        .create-panel {
          width: 100vw;
        }
      }
    `;
    document.head.appendChild(s);
  }
}

customElements.define('crap-create-panel', CrapCreatePanel);

