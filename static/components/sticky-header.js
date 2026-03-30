/**
 * Sticky page header — `<crap-sticky-header>`.
 *
 * Wraps breadcrumb + page title (and optionally toolbar / filter pills)
 * in a sticky container below the main header. Measures its own height
 * via ResizeObserver and publishes `--sticky-header-bottom` on `:root`
 * so sibling sticky elements (e.g. `.edit-layout__sidebar`, table thead)
 * can clear it.
 *
 * Note: `--header-height` is measured globally by an inline script in
 * base.hbs, not by this component.
 *
 * @module sticky-header
 */

class CrapStickyHeader extends HTMLElement {
  connectedCallback() {
    this._ro = new ResizeObserver(() => this._update());
    this._ro.observe(this);
  }

  disconnectedCallback() {
    if (this._ro) this._ro.disconnect();
  }

  _update() {
    const top = parseFloat(getComputedStyle(this).getPropertyValue('top')) || 0;
    const h = this.getBoundingClientRect().height;
    document.documentElement.style.setProperty('--sticky-header-bottom', `${top + h}px`);
  }
}

customElements.define('crap-sticky-header', CrapStickyHeader);
