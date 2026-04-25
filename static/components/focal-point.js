/**
 * Focal point picker — `<crap-focal-point>`.
 *
 * Click the image to set the focal point — a crosshair marker shows
 * the position. Hidden inputs for focal_x/focal_y stay in light DOM
 * for form participation.
 *
 * Attributes:
 *   data-focal-x  — initial X coordinate (0–1, default 0.5)
 *   data-focal-y  — initial Y coordinate (0–1, default 0.5)
 *   data-src      — image source URL
 *
 * @module focal-point
 */

import { t } from './i18n.js';

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
  :host {
    display: block;
    margin-bottom: var(--space-md, 0.75rem);
    text-align: center;
  }

  .focal-point {
    position: relative;
    display: inline-block;
    cursor: crosshair;
  }

  .focal-point img {
    max-width: var(--preview-max-width-lg, 18.75rem);
    max-height: var(--preview-max-width, 12.5rem);
    border-radius: var(--radius-md, 6px);
    object-fit: contain;
    display: block;
  }

  .marker {
    position: absolute;
    width: var(--space-xl, 1.5rem);
    height: var(--space-xl, 1.5rem);
    border: 2px solid var(--bg-elevated, #fff);
    border-radius: 50%;
    box-shadow: 0 0 0 1px rgba(0,0,0,0.3), inset 0 0 0 1px rgba(0,0,0,0.3);
    transform: translate(-50%, -50%);
    pointer-events: none;
    transition: left 0.15s, top 0.15s;
  }

  .hint {
    font-size: var(--text-xs, 0.75rem);
    color: var(--text-tertiary, rgba(0, 0, 0, 0.45));
    margin: var(--space-xs, 0.25rem) 0 0;
  }
`);

class CrapFocalPoint extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    // Read image src from light DOM img or data attribute
    const lightImg = this.querySelector('img');
    const src = lightImg ? lightImg.src : this.dataset.src;
    if (!src) return;

    // Read hidden inputs from light DOM (keep them for form participation)
    let inputX = /** @type {HTMLInputElement|null} */ (
      this.querySelector('input[name="focal_x"]')
    );
    let inputY = /** @type {HTMLInputElement|null} */ (
      this.querySelector('input[name="focal_y"]')
    );

    // Clear light DOM visuals but keep hidden inputs
    const lightChildren = [...this.children];
    for (const child of lightChildren) {
      if (child === inputX || child === inputY) continue;
      child.remove();
    }

    // Build shadow UI — use DOM API for src to prevent XSS via attribute injection
    this.shadowRoot.adoptedStyleSheets = [sheet];
    this.shadowRoot.innerHTML = `
      <div class="focal-point">
        <img alt="" />
        <div class="marker"></div>
      </div>
      <p class="hint"></p>
    `;
    this.shadowRoot.querySelector('img').src = src;
    this.shadowRoot.querySelector('.hint').textContent = t('focal_point_hint');

    const img = this.shadowRoot.querySelector('img');
    const marker = /** @type {HTMLElement} */ (
      this.shadowRoot.querySelector('.marker')
    );

    const rawX = parseFloat(this.dataset.focalX);
    const rawY = parseFloat(this.dataset.focalY);
    const setMarker = (/** @type {number} */ x, /** @type {number} */ y) => {
      marker.style.left = (x * 100) + '%';
      marker.style.top = (y * 100) + '%';
      if (inputX) inputX.value = x.toFixed(4);
      if (inputY) inputY.value = y.toFixed(4);
    };

    setMarker(Number.isNaN(rawX) ? 0.5 : rawX, Number.isNaN(rawY) ? 0.5 : rawY);

    img.addEventListener('click', (e) => {
      const rect = img.getBoundingClientRect();
      const x = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
      const y = Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height));
      setMarker(x, y);
    });
  }

}

customElements.define('crap-focal-point', CrapFocalPoint);
