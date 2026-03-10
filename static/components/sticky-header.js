/**
 * Sticky page header — `<crap-sticky-header>`.
 *
 * Wraps breadcrumb + page title (and optionally toolbar / filter pills)
 * in a sticky container below the main header. Measures its own height
 * via ResizeObserver and publishes `--sticky-header-bottom` on `:root`
 * so sibling sticky elements (e.g. `.edit-layout__sidebar`) can clear it.
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

  /** Publish the bottom edge position as a CSS custom property. */
  _update() {
    const top = parseFloat(getComputedStyle(this).getPropertyValue('top')) || 0;
    const h = this.getBoundingClientRect().height;
    const bottom = top + h;
    const val = `${bottom}px`;

    document.documentElement.style.setProperty('--sticky-header-bottom', val);

    // Directly update edit sidebar top as fallback
    const sidebar = document.querySelector('.edit-layout__sidebar');
    if (sidebar) {
      const gap = parseFloat(
        getComputedStyle(document.documentElement).getPropertyValue('--space-lg'),
      ) || 16;
      sidebar.style.top = `${bottom + gap}px`;
    }
  }
}

customElements.define('crap-sticky-header', CrapStickyHeader);
