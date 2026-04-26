/**
 * Inline Create Panel — `<crap-create-panel>`.
 *
 * Near-fullpage slideout dialog for creating related documents without
 * navigating away from the current edit page. Fetches the existing
 * create form, injects it into a light-DOM `<dialog>`, intercepts
 * submission, and returns the created item to the caller.
 *
 * @example
 *   const panel = getCreatePanel(); // event-based discovery
 *   panel?.open({
 *     collection: 'posts',
 *     title: 'Create Post',
 *     onCreated: ({ id, label }) => { ... },
 *   });
 *
 * @module create-panel
 */

import { css } from './css.js';
import { h, clear } from './h.js';
import { t } from './i18n.js';

/**
 * @typedef {{ id: string, label: string }} CreatedItem
 *
 * @typedef {{
 *   collection: string,
 *   title: string,
 *   onCreated: (item: CreatedItem) => void,
 * }} OpenOptions
 */

const sheet = css`
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
  .create-panel:not([open]) { display: none; }
  .create-panel::backdrop { background: rgba(0, 0, 0, 0.4); }

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
    .create-panel { width: 100vw; }
  }
`;

/** Light-DOM components to unwrap from the embedded form. */
const UNWRAP_TAGS = 'crap-dirty-form, crap-scroll-restore';

/** HTMX attributes to strip so HTMX doesn't intercept submission inside the panel. */
const HTMX_ATTRS = ['hx-post', 'hx-put', 'hx-get', 'hx-target', 'hx-indicator', 'hx-push-url'];

/** @returns {string} */
function readCsrfCookie() {
  const m = document.cookie.match(/(?:^|;\s*)crap_csrf=([^;]*)/);
  if (!m) return '';
  try { return decodeURIComponent(m[1]); } catch { return m[1]; }
}

/**
 * Choose the right body encoding for a fetch:
 *  - `multipart/form-data` (FormData) when any non-empty file is present;
 *  - `application/x-www-form-urlencoded` otherwise — required by the
 *    server's `parse_form` for non-upload collections (axum `Form` extractor
 *    refuses multipart).
 *
 * @param {FormData} formData
 * @returns {{ payload: BodyInit, headers: Record<string, string> }}
 */
function encodeFormBody(formData) {
  const hasFile = [...formData.values()].some((v) => v instanceof File && v.size > 0);
  /** @type {Record<string, string>} */
  const headers = { 'X-Inline-Create': '1' };

  if (hasFile) {
    // Browser sets `multipart/form-data` with boundary automatically.
    return { payload: formData, headers };
  }

  const params = new URLSearchParams();
  for (const [k, v] of formData.entries()) {
    // FormData includes empty File entries for unfilled file inputs — skip them.
    if (v instanceof File) continue;
    params.append(k, v);
  }
  headers['Content-Type'] = 'application/x-www-form-urlencoded;charset=UTF-8';
  return { payload: params, headers };
}

class CrapCreatePanel extends HTMLElement {
  constructor() {
    super();

    /** @type {((item: CreatedItem) => void)|null} */
    this._onCreated = null;
    /** @type {AbortController|null} */
    this._abortController = null;
    /** @type {boolean} */
    this._registered = false;
    /** @type {((e: Event) => void)} */
    this._handleRequest = (e) => {
      const detail = /** @type {CustomEvent} */ (e).detail;
      detail.instance = this;
    };

    /** @type {HTMLHeadingElement} */
    this._titleEl = h('h2', { class: 'create-panel__title' });
    /** @type {HTMLDivElement} */
    this._bodyEl = h('div', { class: 'create-panel__body' });
    /** @type {HTMLDialogElement} */
    this._dialog = h('dialog', { class: 'create-panel' },
      h('div', { class: 'create-panel__header' },
        this._titleEl,
        h('button', {
          type: 'button',
          class: 'create-panel__close',
          'aria-label': t('close'),
          text: '×',
          onClick: () => this.close(),
        }),
      ),
      this._bodyEl,
    );

    this._dialog.addEventListener('click', (e) => {
      if (e.target === this._dialog) this.close();
    });
    this._dialog.addEventListener('cancel', (e) => {
      e.preventDefault();
      this.close();
    });
  }

  connectedCallback() {
    CrapCreatePanel._injectStyles();
    if (!this._dialog.parentNode) this.appendChild(this._dialog);
    if (this._registered) return;
    this._registered = true;
    document.addEventListener('crap:create-panel-request', this._handleRequest);
  }

  disconnectedCallback() {
    if (this._abortController) {
      this._abortController.abort();
      this._abortController = null;
    }
    if (!this._registered) return;
    this._registered = false;
    document.removeEventListener('crap:create-panel-request', this._handleRequest);
  }

  /**
   * Open the panel and load the create form for `opts.collection`.
   *
   * @param {OpenOptions} opts
   */
  async open(opts) {
    this._onCreated = opts.onCreated || null;
    this._titleEl.textContent = opts.title || '';
    this._setBodyMessage('create-panel__loading', t('loading') || 'Loading...');
    this._dialog.showModal();

    if (this._abortController) this._abortController.abort();
    this._abortController = new AbortController();

    try {
      const resp = await fetch(`/admin/collections/${opts.collection}/create`, {
        signal: this._abortController.signal,
        headers: { 'X-Inline-Create': '1' },
      });
      if (!resp.ok) {
        this._setBodyMessage('create-panel__error', t('error') || 'Error');
        return;
      }
      const html = await resp.text();
      this._injectForm(html, opts.collection);
    } catch (e) {
      if (/** @type {Error} */ (e).name === 'AbortError') return;
      this._setBodyMessage('create-panel__error', t('error') || 'Error');
    }
  }

  close() {
    if (this._abortController) this._abortController.abort();
    this._abortController = null;
    this._dialog.close();
    clear(this._bodyEl);
    this._onCreated = null;
  }

  /** @param {string} className @param {string} message */
  _setBodyMessage(className, message) {
    this._bodyEl.replaceChildren(h('p', { class: className, text: message }));
  }

  /**
   * Parse the create-page response and mount the embedded form into
   * the panel.
   *
   * @param {string} html
   * @param {string} collection
   */
  _injectForm(html, collection) {
    // SAFETY: HTML source is the trusted server admin response (same-origin,
    // CSRF-protected, Handlebars-rendered with auto-escaping). NOT user-controlled.
    // Do not adapt this pattern for user content without re-review.
    const doc = new DOMParser().parseFromString(html, 'text/html');
    const form = /** @type {HTMLFormElement|null} */ (doc.querySelector('#edit-form'));
    if (!form) {
      this._setBodyMessage('create-panel__error', t('error') || 'Error');
      return;
    }

    this._unwrapHostComponents(form);
    this._stripHtmxAttrs(form);
    this._reorderEditLayout(form);

    clear(this._bodyEl);
    this._bodyEl.appendChild(form);

    form.addEventListener('submit', (e) => {
      e.preventDefault();
      this._submitForm(form, collection);
    });
  }

  /**
   * Unwrap nested host components (`crap-dirty-form`, `crap-scroll-restore`)
   * by replacing each with its children — we don't want unsaved-changes
   * warnings or scroll restoration inside the panel.
   *
   * @param {HTMLFormElement} form
   */
  _unwrapHostComponents(form) {
    for (const el of form.querySelectorAll(UNWRAP_TAGS)) {
      el.replaceWith(...el.childNodes);
    }
  }

  /** @param {HTMLFormElement} form */
  _stripHtmxAttrs(form) {
    for (const attr of HTMX_ATTRS) form.removeAttribute(attr);
    for (const el of form.querySelectorAll(HTMX_ATTRS.map((a) => `[${a}]`).join(','))) {
      for (const attr of HTMX_ATTRS) el.removeAttribute(attr);
    }
  }

  /**
   * Stack the edit-layout vertically inside the panel and put the
   * sidebar (actions) above the content.
   *
   * The grid override is also applied via CSS `!important`; reordering
   * the children must happen here because CSS can't reorder DOM.
   *
   * @param {HTMLFormElement} form
   */
  _reorderEditLayout(form) {
    const editLayout = form.querySelector('.edit-layout');
    const sidebar = form.querySelector('.edit-layout__sidebar');
    const content = form.querySelector('.edit-layout__content');
    if (editLayout && sidebar && content) {
      editLayout.insertBefore(sidebar, content);
    }
  }

  /**
   * Submit the embedded form via fetch.
   *
   * On success (`X-Created-Id` header present) the caller's `onCreated`
   * fires and the panel closes. On validation error (422) the response
   * body is the re-rendered form, which replaces the current one in the
   * panel.
   *
   * @param {HTMLFormElement} form
   * @param {string} collection
   */
  async _submitForm(form, collection) {
    const submitBtns = /** @type {HTMLButtonElement[]} */ ([
      ...form.querySelectorAll('button[type="submit"], input[type="submit"]'),
    ]);
    const originalLabels = new Map(submitBtns.map((btn) => [btn, btn.textContent]));
    for (const btn of submitBtns) {
      btn.disabled = true;
      btn.textContent = t('saving') || 'Saving...';
    }

    try {
      const resp = await this._sendForm(form, collection);
      await this._handleSubmitResponse(resp, collection);
    } catch {
      this._toast(t('error') || 'Error', 'error');
    } finally {
      for (const btn of submitBtns) {
        btn.disabled = false;
        btn.textContent = originalLabels.get(btn) || '';
      }
    }
  }

  /**
   * @param {HTMLFormElement} form
   * @param {string} collection
   * @returns {Promise<Response>}
   */
  _sendForm(form, collection) {
    const formData = new FormData(form);
    const csrf = readCsrfCookie();
    if (csrf && !formData.has('_csrf')) formData.set('_csrf', csrf);

    const action = form.getAttribute('action') || `/admin/collections/${collection}`;
    const method = (form.getAttribute('method') || 'POST').toUpperCase();
    const { payload, headers } = encodeFormBody(formData);
    if (csrf) headers['X-CSRF-Token'] = csrf;

    return fetch(action, { method, body: payload, headers, redirect: 'manual' });
  }

  /**
   * @param {Response} resp
   * @param {string} collection
   */
  async _handleSubmitResponse(resp, collection) {
    const createdId = resp.headers.get('X-Created-Id');
    if (createdId) {
      const rawLabel = resp.headers.get('X-Created-Label');
      const label = rawLabel ? decodeURIComponent(rawLabel) : createdId;
      if (this._onCreated) this._onCreated({ id: createdId, label });
      this._toast(label, 'success');
      this.close();
      return;
    }

    if (!(resp.ok || resp.status === 422)) return;

    const html = await resp.text();
    this._injectForm(html, collection);

    const toastHeader = resp.headers.get('X-Crap-Toast');
    if (!toastHeader) return;
    try {
      const parsed = JSON.parse(toastHeader);
      this._toast(parsed.message, parsed.type || 'error');
    } catch { /* ignore */ }
  }

  /** @param {string} message @param {'success'|'error'|'info'} type */
  _toast(message, type) {
    document.dispatchEvent(new CustomEvent('crap:toast', { detail: { message, type } }));
  }

  /** @type {boolean} */
  static _stylesInjected = false;

  /** Push the module-level sheet onto `document.adoptedStyleSheets` once per page. */
  static _injectStyles() {
    if (CrapCreatePanel._stylesInjected) return;
    CrapCreatePanel._stylesInjected = true;
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  }
}

customElements.define('crap-create-panel', CrapCreatePanel);
