/**
 * <crap-session-dialog> — Session expiry warning dialog + timer.
 *
 * Reads the `crap_session_exp` cookie (Unix timestamp, set by the server alongside
 * the HttpOnly JWT cookie) to know when the session expires. Sets a timeout for
 * 5 minutes before expiry and shows a styled dialog offering "Stay logged in"
 * (refreshes the session) or "Log out".
 *
 * Instance-safe: all timer logic and event listeners live inside the component.
 * connectedCallback starts monitoring, disconnectedCallback cleans everything up.
 *
 * No polling — just one setTimeout per session, re-scheduled on refresh or
 * HTMX navigation (which may deliver fresh cookies from the server).
 */

import { t } from './i18n.js';

const WARNING_SECONDS = 5 * 60;

/**
 * Read a cookie value by name.
 * @param {string} name
 * @returns {string | null}
 */
function readCookie(name) {
  const match = document.cookie.match(new RegExp('(?:^|;\\s*)' + name + '=([^;]*)'));
  return match ? match[1] : null;
}

// ── Web Component ────────────────────────────────────────────────────────

class CrapSessionDialog extends HTMLElement {
  constructor() {
    super();

    /** @type {number | null} */
    this._timerId = null;
    /** @type {number | null} */
    this._countdownId = null;

    this.attachShadow({ mode: 'open' });
    this.shadowRoot.innerHTML = `
      <style>
        :host {
          display: contents;
        }
        dialog {
          border: none;
          border-radius: var(--radius-xl, 12px);
          padding: 0;
          max-width: 25rem;
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
          padding: var(--space-xl, 1.5rem);
        }
        .body p {
          margin: 0;
          font-size: var(--text-sm, 0.8125rem);
          color: var(--text-primary, rgba(0, 0, 0, 0.88));
          line-height: 1.5;
        }
        .actions {
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

  /** @returns {void} */
  connectedCallback() {
    if (document.querySelector('[data-admin-layout]')) {
      this._scheduleWarning();
    }

    this._handleAfterSettle = () => this._scheduleWarning();
    document.addEventListener('htmx:afterSettle', this._handleAfterSettle);
  }

  /** @returns {void} */
  disconnectedCallback() {
    if (this._timerId !== null) { clearTimeout(this._timerId); this._timerId = null; }
    if (this._countdownId !== null) { clearInterval(this._countdownId); this._countdownId = null; }
    document.removeEventListener('htmx:afterSettle', this._handleAfterSettle);
  }

  /** Schedule (or re-schedule) the session expiry warning. */
  _scheduleWarning() {
    if (this._timerId !== null) {
      clearTimeout(this._timerId);
      this._timerId = null;
    }

    const expStr = readCookie('crap_session_exp');
    if (!expStr) return;

    const exp = parseInt(expStr, 10);
    if (isNaN(exp)) return;

    const nowSec = Math.floor(Date.now() / 1000);
    const secsLeft = exp - nowSec;

    if (secsLeft <= 0) {
      window.location.href = '/admin/login';
      return;
    }

    const warnIn = secsLeft - WARNING_SECONDS;

    if (warnIn <= 0) {
      this._showWarning(secsLeft);
    } else {
      this._timerId = window.setTimeout(() => {
        const freshExp = parseInt(readCookie('crap_session_exp') || '0', 10);
        const freshNow = Math.floor(Date.now() / 1000);
        const freshLeft = freshExp - freshNow;
        if (freshLeft <= 0) {
          window.location.href = '/admin/login';
        } else if (freshLeft <= WARNING_SECONDS) {
          this._showWarning(freshLeft);
        } else {
          this._scheduleWarning();
        }
      }, warnIn * 1000);
    }
  }

  /**
   * Show the warning dialog with a live countdown.
   * @param {number} secsLeft - Seconds remaining before expiry.
   */
  _showWarning(secsLeft) {
    const expStr = readCookie('crap_session_exp');
    const exp = expStr ? parseInt(expStr, 10) : Math.floor(Date.now() / 1000) + secsLeft;

    const updateMessage = () => {
      const nowSec = Math.floor(Date.now() / 1000);
      const remaining = exp - nowSec;
      if (remaining <= 0) {
        if (this._countdownId !== null) { clearInterval(this._countdownId); this._countdownId = null; }
        window.location.href = '/admin/login';
        return;
      }
      const mins = Math.max(1, Math.round(remaining / 60));
      const unit = mins === 1 ? t('minute') : t('minutes');
      const p = this.shadowRoot && this.shadowRoot.querySelector('p');
      if (p) p.textContent = t('session_expiry_warning', { mins, unit });
    };

    updateMessage();
    if (this._countdownId !== null) clearInterval(this._countdownId);
    this._countdownId = window.setInterval(updateMessage, 30_000);

    this.show(t('session_expiry_warning', {
      mins: Math.max(1, Math.round(secsLeft / 60)),
      unit: secsLeft <= 90 ? t('minute') : t('minutes'),
    }), {
      onStay: () => {
        if (this._countdownId !== null) { clearInterval(this._countdownId); this._countdownId = null; }
        this._handleStay();
      },
      onLogout: () => {
        if (this._countdownId !== null) { clearInterval(this._countdownId); this._countdownId = null; }
        this._handleLogout();
      },
    });
  }

  /** POST to the refresh endpoint, then re-schedule. */
  async _handleStay() {
    const csrf = readCookie('crap_csrf');
    try {
      const res = await fetch('/admin/api/session-refresh', {
        method: 'POST',
        headers: csrf ? { 'X-CSRF-Token': csrf } : {},
      });
      if (res.ok) {
        this._scheduleWarning();
      } else {
        window.location.href = '/admin/login';
      }
    } catch {
      window.location.href = '/admin/login';
    }
  }

  /** @returns {void} */
  _handleLogout() {
    window.location.href = '/admin/logout';
  }
}

customElements.define('crap-session-dialog', CrapSessionDialog);
