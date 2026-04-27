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
import { clear, h } from './h.js';
import { t } from './i18n.js';
import { toast } from './util/toast.js';

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

class CrapCreatePanel extends HTMLElement {
  constructor() {
    super();

    /** @type {((item: CreatedItem) => void)|null} */
    this._onCreated = null;
    /** @type {AbortController|null} */
    this._abortController = null;
    /** @type {boolean} */
    this._registered = false;
    /** @type {boolean} */
    this._listenersAttached = false;
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
    this._dialog = h(
      'dialog',
      { class: 'create-panel' },
      h(
        'div',
        { class: 'create-panel__header' },
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
    this._attachPanelResponseListeners();
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
      this._injectForm(html);
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
   */
  _injectForm(html) {
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
    this._wireFormForHtmxSubmit(form);
    this._reorderEditLayout(form);

    clear(this._bodyEl);
    this._bodyEl.appendChild(form);

    // htmx auto-discovery only runs at boot; the embedded form is
    // dynamically inserted, so we tell htmx to scan it for `hx-*`
    // attributes (the form has hx-post + our overrides applied above).
    if (typeof htmx !== 'undefined') htmx.process(form);
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

  /**
   * Override the form's htmx targeting so submission stays in-panel.
   *
   * The server-rendered form already carries `hx-post="…"` (or `-put`)
   * plus `hx-target="body"` (intended for the page-level edit flow,
   * where success swaps the entire body). We override:
   *
   *  - `hx-target="this"` + `hx-swap="outerHTML"` — on a validation
   *    error response htmx replaces the form with the re-rendered
   *    version in-panel; nothing escapes to the parent page.
   *  - `hx-select="#edit-form"` — the validation-error response is the
   *    *full* edit page (the server reuses `collections/edit.hbs`
   *    rather than emitting a fragment); `hx-select` tells htmx to
   *    extract just `<form id="edit-form">` from the body. Without
   *    this, the swap would try to replace the form with `<html>…`
   *    and the panel would melt.
   *  - `hx-headers='{"X-Inline-Create":"1"}'` — the create handler
   *    sees this and returns a panel-friendly response (no
   *    `HX-Redirect`, just `X-Created-Id` / `X-Created-Label`); see
   *    `htmx_inline_created` in `src/admin/handlers/shared/response.rs`.
   *
   * Multipart vs. urlencoded encoding is handled by the form's native
   * `enctype` attribute (which the server template emits as
   * `multipart/form-data` for upload collections, omitted otherwise).
   * htmx 2 reads that attribute to pick the request encoding — the
   * client doesn't have to inspect file inputs at submit time, which
   * is what kept producing the multipart-vs-urlencoded encoding bug
   * in the previous fetch-based path.
   *
   * @param {HTMLFormElement} form
   */
  _wireFormForHtmxSubmit(form) {
    form.setAttribute('hx-target', 'this');
    form.setAttribute('hx-swap', 'outerHTML');
    form.setAttribute('hx-select', '#edit-form');
    form.setAttribute('hx-headers', '{"X-Inline-Create":"1"}');
    form.setAttribute('data-create-panel-form', '1');
    // Drop page-level chrome — irrelevant in-panel.
    form.removeAttribute('hx-push-url');
    form.removeAttribute('hx-indicator');
  }

  /**
   * Listen for response events on the panel body. Listeners live on
   * `_bodyEl` (which never gets replaced) rather than on the form
   * (which gets replaced by `outerHTML` swaps on validation errors),
   * so we attach once and the wiring survives across re-renders.
   *
   * Filtered to events whose target carries
   * `data-create-panel-form` — set in `_wireFormForHtmxSubmit` — so
   * stray htmx events bubbling through (e.g. a future widget's
   * sub-request) don't trigger panel close.
   */
  _attachPanelResponseListeners() {
    if (this._listenersAttached) return;
    this._listenersAttached = true;

    this._bodyEl.addEventListener('htmx:beforeSwap', (evt) => {
      const detail = /** @type {any} */ (evt).detail;
      const target = /** @type {Element|null} */ (evt.target);
      if (!target?.matches?.('[data-create-panel-form]')) return;
      const xhr = /** @type {XMLHttpRequest} */ (detail.xhr);

      // Inline-create success: 2xx with X-Created-Id and empty body.
      // Don't let htmx splice the empty body into the form — we'll
      // close the panel from `htmx:afterRequest` instead.
      if (xhr.getResponseHeader('X-Created-Id')) {
        detail.shouldSwap = false;
      }
    });

    this._bodyEl.addEventListener('htmx:afterRequest', (evt) => {
      const detail = /** @type {any} */ (evt).detail;
      const target = /** @type {Element|null} */ (evt.target);
      if (!target?.matches?.('[data-create-panel-form]')) return;
      const xhr = /** @type {XMLHttpRequest} */ (detail.xhr);

      const createdId = xhr.getResponseHeader('X-Created-Id');
      if (createdId) {
        const rawLabel = xhr.getResponseHeader('X-Created-Label');
        const label = rawLabel ? decodeURIComponent(rawLabel) : createdId;
        if (this._onCreated) this._onCreated({ id: createdId, label });
        toast({ message: label, type: 'success' });
        this.close();
        return;
      }

      // Validation error: 200 OK with re-rendered form body (the swap
      // already happened, htmx extracted #edit-form via hx-select). The
      // server sets `X-Crap-Toast` with the validation summary; surface
      // it so the user sees both the inline field errors and the
      // global "please fix the errors" toast.
      const toastHeader = xhr.getResponseHeader('X-Crap-Toast');
      if (toastHeader) {
        try {
          const parsed = JSON.parse(toastHeader);
          toast({ message: parsed.message, type: parsed.type || 'error' });
          return;
        } catch {
          /* fall through */
        }
      }

      // Network error / non-2xx without a structured response.
      if (!detail.successful) {
        toast({ message: t('error') || 'Error', type: 'error' });
      }
    });

    // After every form swap, re-apply our hx-* overrides to the new
    // form node. htmx's own attribute processing (inherited
    // hx-target / hx-swap) reads from the swapped-in HTML, which
    // carries the *page-level* hx-target="body" — without re-wiring,
    // the next submit would target the parent page body again.
    this._bodyEl.addEventListener('htmx:afterSwap', () => {
      const newForm = /** @type {HTMLFormElement|null} */ (
        this._bodyEl.querySelector('form#edit-form')
      );
      if (newForm) {
        this._wireFormForHtmxSubmit(newForm);
        if (typeof htmx !== 'undefined') htmx.process(newForm);
      }
    });
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
