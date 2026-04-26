/**
 * Toast helper.
 *
 * Dispatches a `crap:toast-request` event that the page-singleton
 * `<crap-toast>` listens for. Replaces the inline
 * `dispatchEvent(new CustomEvent('crap:toast-request', { detail: { message, type } }))`
 * pattern previously duplicated across 4+ component files.
 *
 * @module util/toast
 */

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
  document.dispatchEvent(new CustomEvent('crap:toast-request', { detail }));
}
