/**
 * <crap-session-dialog> — Session expiry warning dialog + timer.
 *
 * Reads the `crap_session_exp` cookie (Unix timestamp, set by the server alongside
 * the HttpOnly JWT cookie) to know when the session expires. Sets a timeout for
 * 5 minutes before expiry and shows a styled dialog offering "Stay logged in"
 * (refreshes the session) or "Log out".
 *
 * No polling — just one setTimeout per session, re-scheduled on refresh or
 * HTMX navigation (which may deliver fresh cookies from the server).
 */

import { t } from './i18n.js';

const WARNING_SECONDS = 5 * 60;

// ── Web Component ────────────────────────────────────────────────────────

class CrapSessionDialog extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    this.shadowRoot.innerHTML = `
      <style>
        :host {
          display: contents;
        }
        dialog {
          border: none;
          border-radius: 12px;
          padding: 0;
          max-width: 400px;
          width: 90vw;
          box-shadow: var(--shadow-lg, 0 16px 48px rgba(0, 0, 0, 0.2));
          font-family: inherit;
          background: var(--bg-elevated, #fff);
          color: var(--text-primary, rgba(0, 0, 0, 0.88));
        }
        dialog::backdrop {
          background: rgba(0, 0, 0, 0.4);
        }
        .body {
          padding: 1.5rem;
        }
        .body p {
          margin: 0;
          font-size: var(--text-sm, 0.875rem);
          color: var(--text-primary, rgba(0, 0, 0, 0.88));
          line-height: 1.5;
        }
        .actions {
          display: flex;
          justify-content: flex-end;
          gap: var(--space-sm, 0.5rem);
          padding: 0 1.5rem 1.5rem;
        }
        button {
          font-family: inherit;
          font-size: var(--text-sm, 0.875rem);
          font-weight: 500;
          padding: 0.5rem 1rem;
          border-radius: var(--radius-md, 6px);
          border: none;
          cursor: pointer;
          transition: background var(--transition-fast, 0.15s ease);
        }
        .logout {
          background: transparent;
          color: var(--text-secondary, rgba(0, 0, 0, 0.65));
          border: 1px solid var(--border-color-hover, #d9d9d9);
        }
        .logout:hover {
          background: var(--bg-hover, rgba(0, 0, 0, 0.04));
        }
        .stay {
          background: var(--color-primary, #1677ff);
          color: var(--text-on-primary, #fff);
        }
        .stay:hover {
          background: var(--color-primary-hover, #4096ff);
        }
      </style>
      <dialog>
        <div class="body">
          <p></p>
        </div>
        <div class="actions">
          <button class="logout" type="button">${t('log_out')}</button>
          <button class="stay" type="button">${t('stay_logged_in')}</button>
        </div>
      </dialog>
    `;
  }

  /**
   * Show the session expiry warning.
   * @param {string} message
   * @param {{ onStay: () => void, onLogout: () => void }} handlers
   */
  show(message, { onStay, onLogout }) {
    const dialog = this.shadowRoot.querySelector('dialog');
    this.shadowRoot.querySelector('p').textContent = message;

    const logoutBtn = this.shadowRoot.querySelector('.logout');
    const stayBtn = this.shadowRoot.querySelector('.stay');

    const cleanup = () => {
      logoutBtn.removeEventListener('click', handleLogout);
      stayBtn.removeEventListener('click', handleStay);
    };

    const handleLogout = () => {
      dialog.close();
      cleanup();
      onLogout();
    };

    const handleStay = () => {
      dialog.close();
      cleanup();
      onStay();
    };

    logoutBtn.addEventListener('click', handleLogout);
    stayBtn.addEventListener('click', handleStay);

    dialog.showModal();
  }

  close() {
    this.shadowRoot.querySelector('dialog').close();
  }
}

customElements.define('crap-session-dialog', CrapSessionDialog);

// ── Cookie reader ────────────────────────────────────────────────────────

/**
 * Read a cookie value by name.
 * @param {string} name
 * @returns {string | null}
 */
function readCookie(name) {
  const match = document.cookie.match(new RegExp('(?:^|;\\s*)' + name + '=([^;]*)'));
  return match ? match[1] : null;
}

// ── Timer logic ──────────────────────────────────────────────────────────

/** @type {number | null} */
let timerId = null;

/** @type {CrapSessionDialog | null} */
let dialogInstance = null;

/** Get or create the shared dialog instance. */
function getDialog() {
  if (!dialogInstance || !dialogInstance.isConnected) {
    dialogInstance = /** @type {CrapSessionDialog} */ (
      document.createElement('crap-session-dialog')
    );
    document.body.appendChild(dialogInstance);
  }
  return dialogInstance;
}

/** Schedule (or re-schedule) the session expiry warning. */
function scheduleWarning() {
  if (timerId !== null) {
    clearTimeout(timerId);
    timerId = null;
  }

  const expStr = readCookie('crap_session_exp');
  if (!expStr) return; // no session

  const exp = parseInt(expStr, 10);
  if (isNaN(exp)) return;

  const nowSec = Math.floor(Date.now() / 1000);
  const secsLeft = exp - nowSec;

  if (secsLeft <= 0) {
    // Already expired — redirect to login
    window.location.href = '/admin/login';
    return;
  }

  const warnIn = secsLeft - WARNING_SECONDS;

  if (warnIn <= 0) {
    // Less than 5 minutes left — show immediately
    showWarning(secsLeft);
  } else {
    timerId = window.setTimeout(() => {
      // Re-read cookie in case it was refreshed by another tab
      const freshExp = parseInt(readCookie('crap_session_exp') || '0', 10);
      const freshNow = Math.floor(Date.now() / 1000);
      const freshLeft = freshExp - freshNow;
      if (freshLeft <= 0) {
        window.location.href = '/admin/login';
      } else if (freshLeft <= WARNING_SECONDS) {
        showWarning(freshLeft);
      } else {
        // Cookie was refreshed — re-schedule
        scheduleWarning();
      }
    }, warnIn * 1000);
  }
}

/**
 * Show the warning dialog.
 * @param {number} secsLeft - Seconds remaining before expiry.
 */
function showWarning(secsLeft) {
  const mins = Math.max(1, Math.round(secsLeft / 60));
  const unit = mins === 1 ? t('minute') : t('minutes');
  const message = t('session_expiry_warning', { mins, unit });

  getDialog().show(message, {
    onStay: handleStay,
    onLogout: handleLogout,
  });
}

/** POST to the refresh endpoint, then re-schedule. */
async function handleStay() {
  const csrf = readCookie('crap_csrf');
  try {
    const res = await fetch('/admin/api/session-refresh', {
      method: 'POST',
      headers: csrf ? { 'X-CSRF-Token': csrf } : {},
    });
    if (res.ok) {
      scheduleWarning();
    } else {
      window.location.href = '/admin/login';
    }
  } catch {
    window.location.href = '/admin/login';
  }
}

function handleLogout() {
  window.location.href = '/admin/logout';
}

// ── Lifecycle ────────────────────────────────────────────────────────────

// Start on page load if we're inside the admin layout
if (document.querySelector('[data-admin-layout]')) {
  scheduleWarning();
}

// Re-schedule after HTMX navigations (server may have refreshed cookies)
document.addEventListener('htmx:afterSettle', () => {
  scheduleWarning();
});
