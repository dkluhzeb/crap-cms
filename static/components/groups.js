/**
 * Collapsible group fields.
 *
 * `toggleGroup` is exported for use as a global (called from inline onclick).
 * State persistence is handled by scroll.js (snapshot before form save,
 * restore after same-page HTMX settle).
 */

/**
 * Toggle a group fieldset's collapsed state.
 *
 * @param {HTMLButtonElement} btn - The toggle button inside the legend.
 */
export function toggleGroup(btn) {
  const fieldset = btn.closest('[data-collapsible]');
  if (!fieldset) return;
  const cls = fieldset.classList.contains('form__collapsible')
    ? 'form__collapsible--collapsed'
    : 'form__group--collapsed';
  fieldset.classList.toggle(cls);
}
