/**
 * Collapsible group/section — `<crap-collapsible>`.
 *
 * Wraps a server-rendered fieldset (form group) or div (collapsible
 * layout block) and toggles the corresponding `--collapsed` modifier
 * class on click of `[data-action="toggle-group"]`. Initial expanded
 * state is server-rendered; persistence across navigation is handled
 * by `<crap-scroll-restore>`.
 *
 * @module groups
 */

/**
 * Map from the parent class on `[data-collapsible]` to the modifier
 * class used to mark the collapsed state.
 */
const COLLAPSED_CLASS = {
  form__collapsible: 'form__collapsible--collapsed',
  form__group: 'form__group--collapsed',
};

class CrapCollapsible extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    this.addEventListener('click', (e) => this._onClick(e));
  }

  /** @param {Event} e */
  _onClick(e) {
    if (!(e.target instanceof Element)) return;
    const btn = e.target.closest('[data-action="toggle-group"]');
    if (!btn) return;
    const fieldset = btn.closest('[data-collapsible]');
    if (!fieldset) return;

    const cls = fieldset.classList.contains('form__collapsible')
      ? COLLAPSED_CLASS.form__collapsible
      : COLLAPSED_CLASS.form__group;
    fieldset.classList.toggle(cls);
    btn.setAttribute('aria-expanded', fieldset.classList.contains(cls) ? 'false' : 'true');
  }
}

customElements.define('crap-collapsible', CrapCollapsible);
