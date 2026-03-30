/**
 * Collapsible group/section — `<crap-collapsible>`.
 *
 * Wraps a group fieldset or collapsible div and handles toggle behavior.
 * State persistence is handled by `<crap-scroll-restore>`.
 *
 * @module groups
 */

class CrapCollapsible extends HTMLElement {
  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    this.addEventListener('click', (e) => {
      const btn = /** @type {HTMLElement} */ (e.target).closest('[data-action="toggle-group"]');
      if (!btn) return;
      const fieldset = btn.closest('[data-collapsible]');
      if (!fieldset) return;
      const cls = fieldset.classList.contains('form__collapsible')
        ? 'form__collapsible--collapsed'
        : 'form__group--collapsed';
      fieldset.classList.toggle(cls);
      const collapsed = fieldset.classList.contains(cls);
      btn.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
    });
  }
}

customElements.define('crap-collapsible', CrapCollapsible);
