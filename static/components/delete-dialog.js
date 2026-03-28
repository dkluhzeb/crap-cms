import { t } from './i18n.js';

/**
 * <crap-delete-dialog> — Singleton delete confirmation dialog.
 *
 * Shows a native `<dialog>` modal with soft-delete / hard-delete options.
 * Submits via fetch() POST and shows toast feedback on completion.
 *
 * Opened programmatically via `window.CrapDeleteDialog.open({ id, title, slug, softDelete, canPermanentlyDelete })`.
 * Also listens for clicks on `[data-delete-id]` buttons via event delegation.
 *
 * @example
 * window.CrapDeleteDialog.open({
 *   id: '123',
 *   title: 'My Post',
 *   slug: 'posts',
 *   softDelete: true,
 *   canPermanentlyDelete: true,
 * });
 */
class CrapDeleteDialog extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    this.shadowRoot.innerHTML = `
      <style>
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

        dialog::backdrop {
          background: rgba(0, 0, 0, 0.4);
        }

        .dialog__body {
          padding: var(--space-xl, 1.5rem);
        }

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

        button:disabled {
          opacity: 0.6;
          cursor: not-allowed;
        }

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
      </style>
      <dialog>
        <div class="dialog__body">
          <p class="dialog__title"></p>
          <p class="dialog__message"></p>
        </div>
        <div class="dialog__actions">
          <button class="btn-cancel" type="button">${t('cancel')}</button>
          <button class="btn-soft" type="button" hidden>${t('move_to_trash')}</button>
          <button class="btn-danger" type="button">${t('delete_permanently')}</button>
        </div>
      </dialog>
    `;
  }

  /** @returns {void} */
  connectedCallback() {
    /** @type {HTMLDialogElement} */
    const dialog = this.shadowRoot.querySelector('dialog');
    /** @type {HTMLParagraphElement} */
    const titleEl = this.shadowRoot.querySelector('.dialog__title');
    /** @type {HTMLParagraphElement} */
    const messageEl = this.shadowRoot.querySelector('.dialog__message');
    /** @type {HTMLButtonElement} */
    const cancelBtn = this.shadowRoot.querySelector('.btn-cancel');
    /** @type {HTMLButtonElement} */
    const softBtn = this.shadowRoot.querySelector('.btn-soft');
    /** @type {HTMLButtonElement} */
    const dangerBtn = this.shadowRoot.querySelector('.btn-danger');

    /**
     * Current dialog state.
     * @type {{ id: string, slug: string, softDelete: boolean, canPermanentlyDelete: boolean } | null}
     * @private
     */
    let pending = null;

    /**
     * Read the CSRF token from the `crap_csrf` cookie.
     * @returns {string}
     */
    const getCsrf = () => {
      const match = document.cookie.match(/(?:^|;\s*)crap_csrf=([^;]*)/);
      return match ? decodeURIComponent(match[1]) : '';
    };

    /**
     * Set disabled state on all action buttons.
     * @param {boolean} disabled
     */
    const setButtonsDisabled = (disabled) => {
      cancelBtn.disabled = disabled;
      softBtn.disabled = disabled;
      dangerBtn.disabled = disabled;
    };

    /**
     * Submit the delete action via fetch.
     * @param {'soft_delete' | 'hard_delete'} action
     * @returns {Promise<void>}
     */
    const submit = async (action) => {
      if (!pending) return;

      const { id, slug } = pending;
      const isEmptyTrash = id === '__empty_trash__';
      const url = isEmptyTrash
        ? `/admin/collections/${slug}/empty-trash`
        : `/admin/collections/${slug}/${id}`;
      const body = new URLSearchParams({
        _csrf: getCsrf(),
        ...(isEmptyTrash ? {} : { _method: 'DELETE', _action: action }),
      });

      setButtonsDisabled(true);

      try {
        const resp = await fetch(url, {
          method: isEmptyTrash ? 'POST' : 'DELETE',
          headers: {
            'Content-Type': 'application/x-www-form-urlencoded',
            'X-Delete-Dialog': '1',
          },
          body,
        });

        dialog.close();
        pending = null;
        setButtonsDisabled(false);

        if (resp.ok) {
          const toastMsg = isEmptyTrash
            ? t('trash_emptied')
            : action === 'soft_delete'
              ? t('moved_to_trash')
              : t('deleted_permanently');
          window.CrapToast.show(toastMsg, 'success');

          // Navigate back to the collection list
          const listUrl = `/admin/collections/${slug}`;
          if (typeof htmx !== 'undefined') {
            htmx.ajax('GET', listUrl, { target: 'body', swap: 'innerHTML' });
            history.pushState({}, '', listUrl);
          } else {
            window.location.href = listUrl;
          }
        } else {
          let errMsg = '';
          try {
            const json = await resp.json();
            errMsg = json.error || '';
          } catch {
            errMsg = await resp.text().catch(() => '');
          }
          window.CrapToast.show(errMsg || t('delete_error'), 'error');
        }
      } catch {
        dialog.close();
        pending = null;
        setButtonsDisabled(false);
        window.CrapToast.show(t('delete_error'), 'error');
      }
    };

    cancelBtn.addEventListener('click', () => {
      pending = null;
      dialog.close();
    });

    dialog.addEventListener('cancel', () => {
      pending = null;
    });

    softBtn.addEventListener('click', () => submit('soft_delete'));
    dangerBtn.addEventListener('click', () => submit('hard_delete'));

    /**
     * Open the delete dialog.
     * @param {{ id: string, title: string, slug: string, softDelete: boolean, canPermanentlyDelete?: boolean }} opts
     * @returns {void}
     */
    this._open = (opts) => {
      const { id, title, slug, softDelete, canPermanentlyDelete = true } = opts;
      pending = { id, slug, softDelete, canPermanentlyDelete };

      const displayTitle = title || id;
      titleEl.textContent = t('delete_confirm_title', { name: displayTitle });
      messageEl.textContent = softDelete
        ? t('delete_confirm_soft')
        : t('delete_confirm_hard');

      softBtn.hidden = !softDelete;
      softBtn.textContent = t('move_to_trash');

      // Hide "Delete permanently" when soft-delete is on but hard-delete is not allowed
      dangerBtn.hidden = softDelete && !canPermanentlyDelete;
      dangerBtn.textContent = t('delete_permanently');

      dialog.showModal();
    };

    // Register for global API requests
    this._handleRequest = (e) => {
      if (!e.detail._handled) {
        e.detail._handled = true;
        this._open(e.detail);
      }
    };
    document.addEventListener('crap:delete-dialog', this._handleRequest);

    // Event delegation: listen for clicks on [data-delete-id] and [data-empty-trash-slug] buttons
    this._handleClick = (e) => {
      // Single document delete
      const deleteBtn = /** @type {HTMLElement | null} */ (
        e.target.closest?.('[data-delete-id]')
      );
      if (deleteBtn) {
        e.preventDefault();
        e.stopPropagation();
        this._open({
          id: deleteBtn.dataset.deleteId,
          title: deleteBtn.dataset.deleteTitle || '',
          slug: deleteBtn.dataset.deleteSlug,
          softDelete: deleteBtn.dataset.deleteSoft === 'true',
          canPermanentlyDelete: deleteBtn.dataset.deleteCanPerm !== 'false',
        });
        return;
      }

      // Empty trash
      const emptyBtn = /** @type {HTMLElement | null} */ (
        e.target.closest?.('[data-empty-trash-slug]')
      );
      if (emptyBtn) {
        e.preventDefault();
        e.stopPropagation();
        const slug = emptyBtn.dataset.emptyTrashSlug;
        const count = emptyBtn.dataset.emptyTrashCount || '?';

        titleEl.textContent = t('empty_trash_confirm_title');
        messageEl.textContent = t('empty_trash_confirm', { count });
        softBtn.hidden = true;
        dangerBtn.textContent = t('empty_trash');
        pending = { id: '__empty_trash__', slug, softDelete: false };
        dialog.showModal();
      }
    };
    document.addEventListener('click', this._handleClick);
  }

  /** @returns {void} */
  disconnectedCallback() {
    document.removeEventListener('crap:delete-dialog', this._handleRequest);
    document.removeEventListener('click', this._handleClick);
  }
}

customElements.define('crap-delete-dialog', CrapDeleteDialog);

/**
 * Global delete dialog API.
 * Dispatches a CustomEvent that the connected <crap-delete-dialog> instance handles.
 * @namespace
 */
window.CrapDeleteDialog = {
  /**
   * Open the delete confirmation dialog.
   * @param {{ id: string, title: string, slug: string, softDelete: boolean, canPermanentlyDelete?: boolean }} opts
   * @returns {void}
   */
  open(opts) {
    document.dispatchEvent(new CustomEvent('crap:delete-dialog', {
      detail: opts,
    }));
  },
};
