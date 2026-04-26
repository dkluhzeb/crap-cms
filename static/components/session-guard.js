/**
 * Session expiry warning — `<crap-session-dialog>`.
 *
 * Reads the `crap_session_exp` cookie (Unix-second timestamp the server
 * sets alongside the HttpOnly JWT cookie) to know when the session
 * expires. Schedules **one** `setTimeout` for `WARNING_SECONDS` before
 * expiry — no polling. The timer is re-scheduled on every HTMX
 * navigation, since server responses may carry a fresh cookie.
 *
 * On warning the dialog offers:
 *  - **Stay logged in** — POST `/admin/api/session-refresh`, re-schedule.
 *  - **Log out** — navigate to `LOGOUT_URL`.
 *
 * @module session-guard
 */

import { css } from './css.js';
import { h } from './h.js';
import { t } from './i18n.js';
import { readCookie, readCsrfCookie } from './util/cookies.js';

/** Show the warning this many seconds before expiry. */
const WARNING_SECONDS = 5 * 60;

/** Refresh the countdown text every 30s while the dialog is open. */
const COUNTDOWN_TICK_MS = 30_000;

const LOGIN_URL = '/admin/login';
const LOGOUT_URL = '/admin/logout';
const REFRESH_URL = '/admin/api/session-refresh';

const sheet = css`
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
`;

/** Read the session expiry timestamp from the cookie, or `null` if absent/malformed. */
function readSessionExpiry() {
  const raw = readCookie('crap_session_exp');
  if (!raw) return null;
  const exp = Number.parseInt(raw, 10);
  return Number.isNaN(exp) ? null : exp;
}

/** @returns {number} Current Unix-second timestamp. */
function nowSec() {
  return Math.floor(Date.now() / 1000);
}

/**
 * Format the "stay logged in" message body for `secsLeft` remaining.
 * @param {number} secsLeft
 */
function expiryMessage(secsLeft) {
  const mins = Math.max(1, Math.round(secsLeft / 60));
  const unit = mins === 1 ? t('minute') : t('minutes');
  return t('session_expiry_warning', { mins, unit });
}

class CrapSessionDialog extends HTMLElement {
  constructor() {
    super();

    /** @type {boolean} */
    this._connected = false;
    /** @type {ReturnType<typeof setTimeout>|null} */
    this._timerId = null;
    /** @type {ReturnType<typeof setInterval>|null} */
    this._countdownId = null;
    /** @type {(() => void)|null} */
    this._onAfterSettle = null;

    const root = this.attachShadow({ mode: 'open' });
    root.adoptedStyleSheets = [sheet];

    /** @type {HTMLParagraphElement} */
    this._messageEl = h('p');
    /** @type {HTMLButtonElement} */
    this._logoutBtn = h('button', { class: 'logout', type: 'button', text: t('log_out') });
    /** @type {HTMLButtonElement} */
    this._stayBtn = h('button', { class: 'stay', type: 'button', text: t('stay_logged_in') });
    /** @type {HTMLDialogElement} */
    this._dialog = h(
      'dialog',
      null,
      h('div', { class: 'body' }, this._messageEl),
      h('div', { class: 'actions' }, this._logoutBtn, this._stayBtn),
    );
    root.append(this._dialog);
  }

  /* ── Lifecycle ──────────────────────────────────────────────── */

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    if (document.querySelector('[data-admin-layout]')) this._scheduleWarning();
    this._onAfterSettle = () => this._scheduleWarning();
    document.addEventListener('htmx:afterSettle', this._onAfterSettle);
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    this._cancelTimers();
    if (this._onAfterSettle) document.removeEventListener('htmx:afterSettle', this._onAfterSettle);
  }

  _cancelTimers() {
    if (this._timerId != null) {
      clearTimeout(this._timerId);
      this._timerId = null;
    }
    if (this._countdownId != null) {
      clearInterval(this._countdownId);
      this._countdownId = null;
    }
  }

  /* ── Public dialog API ──────────────────────────────────────── */

  /**
   * Show the warning dialog. The two button handlers self-clean via an
   * `AbortController` — when the controller aborts, all listeners (incl.
   * dialog `cancel`) are removed in one step.
   *
   * @param {string} message
   * @param {{ onStay: () => void, onLogout: () => void }} handlers
   */
  show(message, { onStay, onLogout }) {
    this._messageEl.textContent = message;

    const ctrl = new AbortController();
    const settle = (fn) => {
      ctrl.abort();
      fn();
    };

    this._logoutBtn.addEventListener(
      'click',
      () => {
        this._dialog.close();
        settle(onLogout);
      },
      { signal: ctrl.signal },
    );

    this._stayBtn.addEventListener(
      'click',
      () => {
        this._dialog.close();
        settle(onStay);
      },
      { signal: ctrl.signal },
    );

    // ESC fires `cancel`. Treat as "stay" — opt-out of accidental logout.
    this._dialog.addEventListener(
      'cancel',
      (e) => {
        e.preventDefault();
        this._dialog.close();
        settle(onStay);
      },
      { signal: ctrl.signal },
    );

    this._dialog.showModal();
  }

  close() {
    this._dialog.close();
  }

  /* ── Scheduling ─────────────────────────────────────────────── */

  /** Schedule (or re-schedule) the session expiry warning. */
  _scheduleWarning() {
    if (this._timerId != null) {
      clearTimeout(this._timerId);
      this._timerId = null;
    }

    const exp = readSessionExpiry();
    if (exp == null) return;

    const secsLeft = exp - nowSec();
    if (secsLeft <= 0) {
      window.location.href = LOGIN_URL;
      return;
    }

    const warnIn = secsLeft - WARNING_SECONDS;
    if (warnIn <= 0) {
      this._showWarning(secsLeft);
      return;
    }

    this._timerId = setTimeout(() => this._onWarnTimerFired(), warnIn * 1000);
  }

  /**
   * Re-check the cookie when the warning timer fires (the cookie may
   * have been refreshed by an HTMX request in the meantime).
   */
  _onWarnTimerFired() {
    const exp = readSessionExpiry();
    if (exp == null) return;
    const left = exp - nowSec();
    if (left <= 0) {
      window.location.href = LOGIN_URL;
      return;
    }
    if (left <= WARNING_SECONDS) {
      this._showWarning(left);
      return;
    }
    this._scheduleWarning();
  }

  /**
   * Show the warning dialog with a live countdown.
   *
   * @param {number} secsLeft Seconds remaining before expiry.
   */
  _showWarning(secsLeft) {
    const exp = readSessionExpiry() ?? nowSec() + secsLeft;
    this._startCountdown(exp);

    this.show(expiryMessage(secsLeft), {
      onStay: () => {
        this._cancelCountdown();
        this._handleStay();
      },
      onLogout: () => {
        this._cancelCountdown();
        this._handleLogout();
      },
    });
  }

  /**
   * Tick the countdown copy every {@link COUNTDOWN_TICK_MS}. Auto-redirect
   * to login if expiry is reached while the dialog is open.
   *
   * @param {number} exp Unix-second timestamp the session expires at.
   */
  _startCountdown(exp) {
    this._cancelCountdown();
    const tick = () => {
      const remaining = exp - nowSec();
      if (remaining <= 0) {
        this._cancelCountdown();
        window.location.href = LOGIN_URL;
        return;
      }
      this._messageEl.textContent = expiryMessage(remaining);
    };
    tick();
    this._countdownId = setInterval(tick, COUNTDOWN_TICK_MS);
  }

  _cancelCountdown() {
    if (this._countdownId != null) {
      clearInterval(this._countdownId);
      this._countdownId = null;
    }
  }

  /* ── Stay / logout handlers ─────────────────────────────────── */

  /** POST the refresh endpoint, then re-schedule. */
  async _handleStay() {
    const csrf = readCsrfCookie();
    try {
      const res = await fetch(REFRESH_URL, {
        method: 'POST',
        headers: csrf ? { 'X-CSRF-Token': csrf } : {},
      });
      if (res.ok) {
        this._scheduleWarning();
        return;
      }
    } catch {
      /* fall through */
    }
    window.location.href = LOGIN_URL;
  }

  _handleLogout() {
    window.location.href = LOGOUT_URL;
  }
}

customElements.define('crap-session-dialog', CrapSessionDialog);
