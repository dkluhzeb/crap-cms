/**
 * Leaving an edit form from outside it.
 *
 * A component that reloads the page itself (the editor and UI locale
 * pickers) must ask the edit form's `<crap-dirty-form>` guard BEFORE it
 * changes anything: a preference written first survives the editor's
 * "Stay", and the form on screen no longer matches it.
 *
 * @module leave
 * @stability internal
 */

/**
 * Ask the page's edit-form guard whether to leave: resolves `true` when
 * there is no guard, nothing is unsaved, or the editor chose to leave;
 * `false` when they chose to stay.
 *
 * @returns {Promise<boolean>}
 */
export function confirmLeave() {
  const guard = /** @type {(HTMLElement & { confirmLeave?: () => Promise<boolean> })|null} */ (
    document.querySelector('crap-dirty-form')
  );
  return guard?.confirmLeave ? guard.confirmLeave() : Promise.resolve(true);
}
