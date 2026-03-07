/**
 * Collapsible group fields.
 *
 * Registers a `toggle-group` action via the delegation system.
 * State persistence is handled by scroll.js (snapshot before form save,
 * restore after same-page HTMX settle).
 */

import { registerAction } from './actions.js';

/**
 * Toggle a group fieldset's collapsed state.
 *
 * @param {HTMLButtonElement} btn - The toggle button inside the legend.
 */
function toggleGroup(btn) {
  const fieldset = btn.closest('[data-collapsible]');
  if (!fieldset) return;
  const cls = fieldset.classList.contains('form__collapsible')
    ? 'form__collapsible--collapsed'
    : 'form__group--collapsed';
  fieldset.classList.toggle(cls);
  const collapsed = fieldset.classList.contains(cls);
  btn.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
}

registerAction('toggle-group', (el) => toggleGroup(el));
