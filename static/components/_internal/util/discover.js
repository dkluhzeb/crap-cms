/**
 * Singleton-component discovery.
 *
 * Singletons (`<crap-drawer>`, `<crap-confirm-dialog>`,
 * `<crap-create-panel>`, `<crap-delete-dialog>`) advertise themselves
 * by listening for a `crap:<name>-request` event and writing
 * `detail.instance = this`. Callers dispatch the event, then pull the
 * instance off `detail`.
 *
 * This util encapsulates the dance.
 *
 * @module util/discover
 * @stability internal
 */

/**
 * Discover the page-singleton instance for a component.
 *
 * @param {string} eventName e.g. `'crap:drawer-request'`.
 * @returns {any|null} The component instance, or `null` if no
 *   instance is currently mounted on the page.
 */
export function discoverSingleton(eventName) {
  const evt = new CustomEvent(eventName, { detail: {} });
  document.dispatchEvent(evt);
  return /** @type {any} */ (evt).detail.instance ?? null;
}
