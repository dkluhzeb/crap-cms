/**
 * <crap-delete-dialog> — Singleton delete confirmation dialog.
 *
 * Shows a native `<dialog>` modal with soft-delete / hard-delete options
 * (and a separate empty-trash variant). Submits via `fetch` and toasts
 * the result.
 *
 * Three ways to open it:
 *   - **Sugar**: `window.crap.deleteDialog.open(opts)` (preferred).
 *   - **Discovery**: dispatch `crap:delete-dialog-request`, read
 *     `detail.instance`, call `instance.open(opts)`.
 *   - **Event-delegated**: any `[data-delete-id]` or
 *     `[data-empty-trash-slug]` button click anywhere in the document.
 *
 * @example
 * window.crap.deleteDialog.open({
 *   id: '123', title: 'My Post', slug: 'posts',
 *   softDelete: true, canPermanentlyDelete: true,
 * });
 *
 * @module delete-dialog
 */

import { css } from './css.js';
import { h } from './h.js';
import { t } from './i18n.js';
import { readCsrfCookie } from './util/cookies.js';
import { toast } from './util/toast.js';

const sheet = css`
  :host { display: contents; }

  dialog {
    border: none;
    border-radius: var(--radius-xl, 12px);
    padding: 0;
    max-width: 26.25rem;
    width: 90vw;
    box-shadow: var(--shadow-lg, 0 16px 48px rgba(0, 0, 0, 0.2));
    font-family: inherit;
    background: var(--bg-elevated, #fff);
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
  }
  dialog::backdrop { background: rgba(0, 0, 0, 0.4); }

  .dialog__body { padding: var(--space-xl, 1.5rem); }
  .dialog__title {
    margin: 0 0 var(--space-sm, 0.5rem);
    font-size: var(--text-base, 0.875rem);
    font-weight: 600;
    color: var(--text-primary, rgba(0, 0, 0, 0.88));
  }
  .dialog__message {
    margin: 0;
    font-size: var(--text-sm, 0.8125rem);
    color: var(--text-secondary, rgba(0, 0, 0, 0.65));
    line-height: 1.5;
  }

  .dialog__actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-sm, 0.5rem);
    padding: 0 var(--space-xl, 1.5rem) var(--space-xl, 1.5rem);
  }

  button {
    font-family: inherit;
    font-size: var(--text-sm, 0.8125rem);
    font-weight: 500;
    height: var(--button-height, 2.25rem);
    padding: 0 var(--space-lg, 1rem);
    border-radius: var(--radius-md, 6px);
    border: none;
    cursor: pointer;
    transition: background var(--transition-fast, 0.15s ease);
  }
  button:disabled { opacity: 0.6; cursor: not-allowed; }

  .btn-cancel {
    background: transparent;
    color: var(--text-secondary, rgba(0, 0, 0, 0.65));
    border: 1px solid var(--border-color-hover, #d9d9d9);
  }
  .btn-cancel:hover:not(:disabled) {
    background: var(--bg-hover, rgba(0, 0, 0, 0.04));
  }

  .btn-soft {
    background: var(--color-primary, #1677ff);
    color: var(--text-on-primary, #fff);
  }
  .btn-soft:hover:not(:disabled) {
    background: var(--color-primary-hover, #4096ff);
  }

  .btn-danger {
    background: var(--color-danger, #dc2626);
    color: var(--text-on-primary, #fff);
  }
  .btn-danger:hover:not(:disabled) {
    background: var(--color-danger-hover, #ef4444);
  }
`;

/**
 * @typedef {{
 *   id: string,
 *   title?: string,
 *   slug: string,
 *   softDelete: boolean,
 *   canPermanentlyDelete?: boolean,
 * }} OpenOptions
 *
 * @typedef {{ id: string, slug: string, softDelete: boolean }} PendingState
 */

/** Sentinel id used by the empty-trash flow. */
const EMPTY_TRASH_ID = '__empty_trash__';

/**
 * Pick the success-toast message for a completed delete.
 *
 * @param {boolean} isEmptyTrash
 * @param {'soft_delete' | 'hard_delete' | null} action
 */
function successMessage(isEmptyTrash, action) {
  if (isEmptyTrash) return t('trash_emptied');
  if (action === 'soft_delete') return t('moved_to_trash');
  return t('deleted_permanently');
}

class CrapDeleteDialog extends HTMLElement {
  constructor() {
    super();

    /** @type {boolean} */
    this._connected = false;
    /** @type {boolean} */
    this._submitting = false;
    /** @type {PendingState|null} */
    this._pending = null;
    /** @type {((e: Event) => void)|null} */
    this._handleRequest = null;
    /** @type {((e: Event) => void)|null} */
    this._handleClick = null;

    const root = this.attachShadow({ mode: 'open' });
    root.adoptedStyleSheets = [sheet];

    /** @type {HTMLParagraphElement} */
    this._titleEl = h('p', { class: 'dialog__title' });
    /** @type {HTMLParagraphElement} */
    this._messageEl = h('p', { class: 'dialog__message' });
    /** @type {HTMLButtonElement} */
    this._cancelBtn = h('button', { class: 'btn-cancel', type: 'button', text: t('cancel') });
    /** @type {HTMLButtonElement} */
    this._softBtn = h('button', {
      class: 'btn-soft',
      type: 'button',
      hidden: true,
      text: t('move_to_trash'),
    });
    /** @type {HTMLButtonElement} */
    this._dangerBtn = h('button', {
      class: 'btn-danger',
      type: 'button',
      text: t('delete_permanently'),
    });
    /** @type {HTMLDialogElement} */
    this._dialog = h(
      'dialog',
      null,
      h('div', { class: 'dialog__body' }, this._titleEl, this._messageEl),
      h('div', { class: 'dialog__actions' }, this._cancelBtn, this._softBtn, this._dangerBtn),
    );
    root.append(this._dialog);

    this._cancelBtn.addEventListener('click', () => this._cancel());
    this._dialog.addEventListener('cancel', () => {
      this._pending = null;
    });
    this._softBtn.addEventListener('click', () => this._submit('soft_delete'));
    this._dangerBtn.addEventListener('click', () => this._submit('hard_delete'));
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    this._handleRequest = (e) => {
      const detail = /** @type {CustomEvent} */ (e).detail;
      if (!detail.instance) detail.instance = this;
    };
    this._handleClick = (e) => this._onDocumentClick(e);

    document.addEventListener('crap:delete-dialog-request', this._handleRequest);
    document.addEventListener('click', this._handleClick);
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    if (this._handleRequest) {
      document.removeEventListener('crap:delete-dialog-request', this._handleRequest);
    }
    if (this._handleClick) {
      document.removeEventListener('click', this._handleClick);
    }
  }

  /**
   * Open the dialog with delete options for one document.
   *
   * @param {OpenOptions} opts
   */
  open(opts) {
    const { id, title, slug, softDelete, canPermanentlyDelete = true } = opts;
    this._pending = { id, slug, softDelete };

    this._titleEl.textContent = t('delete_confirm_title', { name: title || id });
    this._messageEl.textContent = softDelete ? t('delete_confirm_soft') : t('delete_confirm_hard');

    this._softBtn.hidden = !softDelete;
    this._softBtn.textContent = t('move_to_trash');
    // Hide "Delete permanently" when soft-delete is on AND hard-delete is forbidden.
    this._dangerBtn.hidden = softDelete && !canPermanentlyDelete;
    this._dangerBtn.textContent = t('delete_permanently');

    this._dialog.showModal();
  }

  /**
   * Open the dialog in empty-trash mode for `slug`.
   *
   * @param {{ slug: string, count?: string }} opts
   */
  openEmptyTrash({ slug, count = '?' }) {
    this._pending = { id: EMPTY_TRASH_ID, slug, softDelete: false };
    this._titleEl.textContent = t('empty_trash_confirm_title');
    this._messageEl.textContent = t('empty_trash_confirm', { count });
    this._softBtn.hidden = true;
    this._dangerBtn.hidden = false;
    this._dangerBtn.textContent = t('empty_trash');
    this._dialog.showModal();
  }

  _cancel() {
    this._pending = null;
    this._dialog.close();
  }

  /**
   * @param {Event} e
   */
  _onDocumentClick(e) {
    const target = e.target;
    if (!(target instanceof Element)) return;

    const deleteBtn = /** @type {HTMLElement|null} */ (target.closest('[data-delete-id]'));
    if (deleteBtn) {
      e.preventDefault();
      e.stopPropagation();
      this.open({
        id: deleteBtn.dataset.deleteId || '',
        title: deleteBtn.dataset.deleteTitle || '',
        slug: deleteBtn.dataset.deleteSlug || '',
        softDelete: deleteBtn.dataset.deleteSoft === 'true',
        canPermanentlyDelete: deleteBtn.dataset.deleteCanPerm !== 'false',
      });
      return;
    }

    const emptyBtn = /** @type {HTMLElement|null} */ (target.closest('[data-empty-trash-slug]'));
    if (!emptyBtn) return;
    e.preventDefault();
    e.stopPropagation();
    this.openEmptyTrash({
      slug: emptyBtn.dataset.emptyTrashSlug || '',
      count: emptyBtn.dataset.emptyTrashCount,
    });
  }

  /**
   * Submit the delete action. Toasts on success and navigates back to
   * the collection list; toasts the server error message on failure.
   *
   * @param {'soft_delete' | 'hard_delete'} action
   */
  async _submit(action) {
    if (!this._pending || this._submitting) return;
    this._submitting = true;
    this._setButtonsDisabled(true);

    const pending = this._pending;
    const isEmptyTrash = pending.id === EMPTY_TRASH_ID;

    try {
      const resp = await fetch(this._buildUrl(pending, isEmptyTrash), {
        method: isEmptyTrash ? 'POST' : 'DELETE',
        headers: {
          'Content-Type': 'application/x-www-form-urlencoded',
          'X-Delete-Dialog': '1',
        },
        body: this._buildBody(action, isEmptyTrash),
      });

      if (resp.ok) {
        toast({ message: successMessage(isEmptyTrash, action), type: 'success' });
        this._navigateToList(pending.slug);
        return;
      }

      toast({ message: await this._readErrorMessage(resp), type: 'error' });
    } catch {
      toast({ message: t('delete_error'), type: 'error' });
    } finally {
      this._dialog.close();
      this._pending = null;
      this._submitting = false;
      this._setButtonsDisabled(false);
    }
  }

  /** @param {boolean} disabled */
  _setButtonsDisabled(disabled) {
    this._cancelBtn.disabled = disabled;
    this._softBtn.disabled = disabled;
    this._dangerBtn.disabled = disabled;
  }

  /** @param {PendingState} pending @param {boolean} isEmptyTrash */
  _buildUrl(pending, isEmptyTrash) {
    return isEmptyTrash
      ? `/admin/collections/${pending.slug}/empty-trash`
      : `/admin/collections/${pending.slug}/${pending.id}`;
  }

  /** @param {'soft_delete' | 'hard_delete'} action @param {boolean} isEmptyTrash */
  _buildBody(action, isEmptyTrash) {
    const params = new URLSearchParams({ _csrf: readCsrfCookie() });
    if (!isEmptyTrash) {
      params.set('_method', 'DELETE');
      params.set('_action', action);
    }
    return params;
  }

  /**
   * Decode the server's error message from a non-OK response. Returns
   * the localised fallback if the body isn't JSON or has no `error` key.
   *
   * @param {Response} resp
   */
  async _readErrorMessage(resp) {
    try {
      const json = JSON.parse(await resp.text());
      return json.error || t('delete_error');
    } catch {
      return t('delete_error');
    }
  }

  /** @param {string} slug */
  _navigateToList(slug) {
    const url = `/admin/collections/${slug}`;
    if (typeof htmx !== 'undefined') {
      htmx.ajax('GET', url, { target: 'body', swap: 'innerHTML' });
      history.pushState({}, '', url);
      return;
    }
    window.location.href = url;
  }
}

customElements.define('crap-delete-dialog', CrapDeleteDialog);
