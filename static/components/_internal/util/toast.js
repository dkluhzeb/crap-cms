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
