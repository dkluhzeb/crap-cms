/**
 * Sticky page header — `<crap-sticky-header>`.
 *
 * Wraps breadcrumb + page title (and optionally toolbar / filter pills)
 * in a sticky container below the main header. Measures its own height
 * via `ResizeObserver` and publishes `--sticky-header-bottom` on
 * `:root` so sibling sticky elements (e.g. `.edit-layout__sidebar`,
 * table thead) can clear it.
 *
 * Note: `--header-height` is measured globally by an inline script in
 * `base.hbs`, not by this component.
 *
 * @module sticky-header
 */

class CrapStickyHeader extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {ResizeObserver|null} */
    this._observer = null;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    this._observer = new ResizeObserver(() => this._publishHeight());
    this._observer.observe(this);
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    this._observer?.disconnect();
    this._observer = null;
  }

  /**
   * Push our `top + height` to a `:root` custom property so other
   * sticky descendants can offset themselves below us.
   */
  _publishHeight() {
    const top = Number.parseFloat(getComputedStyle(this).getPropertyValue('top')) || 0;
    const height = this.getBoundingClientRect().height;
    document.documentElement.style.setProperty('--sticky-header-bottom', `${top + height}px`);
  }
}

customElements.define('crap-sticky-header', CrapStickyHeader);
