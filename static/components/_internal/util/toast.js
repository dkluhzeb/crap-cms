/**
 * Toast helper.
 *
 * Dispatches the {@link EV_TOAST_REQUEST} event that the page-singleton
 * `<crap-toast>` listens for. Replaces the inline
 * `dispatchEvent(new CustomEvent('crap:toast-request', …))`
 * pattern previously duplicated across 4+ component files.
 *
 * @module util/toast
 * @stability internal
 */

import { EV_TOAST_REQUEST } from '../../events.js';

/**
 * @typedef {'success' | 'error' | 'info'} ToastType
 *
 * @typedef {{ message: string, type?: ToastType, duration?: number }} ToastDetail
 */

/**
 * Show a toast notification.
 *
 * @param {ToastDetail} detail
 */
export function toast(detail) {
  document.dispatchEvent(new CustomEvent(EV_TOAST_REQUEST, { detail }));
}

/**
 * Read an `X-Crap-Toast` header value: a JSON `{ message, type? }`, or a
 * plain-text message. `null` for a missing or empty header.
 *
 * @param {string|null} header
 * @returns {{ message: string, type?: ToastType }|null}
 */
export function parseToastHeader(header) {
  if (!header) return null;
  try {
    const data = JSON.parse(header);
    if (data && typeof data.message === 'string') return data;
  } catch {
    /* plain-text header */
  }
  return { message: header };
}

/**
 * Toast the failure of a `fetch` the server refused: the message its
 * `X-Crap-Toast` header carries (every admin error response sets one), or
 * `fallback` when it carries none.
 *
 * @param {Response} resp
 * @param {string} fallback
 */
export function toastFailedResponse(resp, fallback) {
  const parsed = parseToastHeader(resp.headers.get('X-Crap-Toast'));
  toast({ message: parsed?.message || fallback, type: 'error' });
}
