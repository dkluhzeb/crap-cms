/**
 * Live event stream (SSE) — real-time mutation notifications.
 *
 * Connects to /admin/events and shows toast notifications when documents
 * are created, updated, or deleted. Auto-reconnects on connection loss.
 *
 * When on an edit page, detects if another user modified the same document
 * and shows a persistent stale content warning banner.
 */

if (typeof EventSource !== 'undefined') {
  /** @type {EventSource | null} */
  let source = null;

  /** Timestamp of last form save in this tab (to distinguish own saves from other tabs). */
  let lastSaveTime = 0;
  const SAVE_GRACE_MS = 5000;

  document.addEventListener('htmx:beforeRequest', /** @param {CustomEvent} e */ (e) => {
    if (e.detail.requestConfig.verb !== 'get') {
      lastSaveTime = Date.now();
    }
  });

  /**
   * Show or update a stale content warning banner on the edit form.
   * @param {'updated' | 'deleted'} action
   * @param {{ id: string, email: string } | null} editedBy
   */
  function showStaleWarning(action, editedBy) {
    const form = document.getElementById('edit-form');
    if (!form) return;

    const isDeleted = action === 'deleted';
    const who = editedBy ? editedBy.email : 'another user';

    // Reuse existing banner or create new one
    let banner = document.getElementById('stale-warning');
    if (!banner) {
      banner = document.createElement('div');
      banner.id = 'stale-warning';
      banner.className = 'stale-warning';
      form.parentNode.insertBefore(banner, form);
    }

    const message = isDeleted
      ? `This document was deleted by ${who}.`
      : `This document was updated by ${who}. Your content may be outdated.`;

    banner.innerHTML = `
      <span class="stale-warning__icon">&#9888;</span>
      <span class="stale-warning__text">${message}</span>
      <span class="stale-warning__actions">
        ${isDeleted ? '' : '<button type="button" class="stale-warning__reload button button--ghost button--small">Reload</button>'}
        <button type="button" class="stale-warning__dismiss">&times;</button>
      </span>
    `;

    // Bind reload
    const reloadBtn = banner.querySelector('.stale-warning__reload');
    if (reloadBtn) {
      reloadBtn.onclick = () => location.reload();
    }

    // Bind dismiss
    const dismissBtn = banner.querySelector('.stale-warning__dismiss');
    if (dismissBtn) {
      dismissBtn.onclick = () => banner.remove();
    }

    // For delete: disable all form inputs
    if (isDeleted) {
      form.querySelectorAll('input, select, textarea, button[type="submit"]').forEach(el => {
        el.disabled = true;
      });
    }
  }

  function connect() {
    source = new EventSource('/admin/events');

    source.addEventListener('mutation', /** @param {MessageEvent} e */ (e) => {
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

          // Skip if this is our own save from this tab (within grace window).
          // Same user editing in another tab will still trigger the warning.
          const isSelf = currentUserId && event.edited_by && event.edited_by.id === currentUserId;
          const isOwnSave = isSelf && (Date.now() - lastSaveTime < SAVE_GRACE_MS);
          if (isCurrentDoc && (op === 'delete' || op === 'update') && !isOwnSave) {
            showStaleWarning(op === 'delete' ? 'deleted' : 'updated', event.edited_by || null);
            return;
          }
        }

        /** @type {Record<string, string>} */
        const opLabels = {
          create: 'created',
          update: 'updated',
          delete: 'deleted',
        };
        const action = opLabels[op] || op;
        const msg = `${collection} ${action}`;

        if (window.CrapToast) {
          window.CrapToast.show(msg, 'info');
        }
      } catch (err) {
        // Ignore parse errors
      }
    });

    source.onerror = () => {
      if (source && source.readyState === EventSource.CLOSED) {
        source = null;
        setTimeout(connect, 5000);
      }
    };
  }

  // Only connect on admin pages (not login/logout)
  if (document.querySelector('[data-admin-layout]')) {
    connect();
  }
}
