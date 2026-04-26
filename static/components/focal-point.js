/**
 * Focal point picker — `<crap-focal-point>`.
 *
 * Composes a slotted `<img>` (the preview) with a crosshair marker
 * rendered above it inside the shadow root. Clicking the image moves
 * the marker and writes the normalised `[0, 1]` coordinates into the
 * slotted hidden inputs `focal_x` / `focal_y` so they submit with the
 * surrounding form.
 *
 * The img and inputs stay in light DOM (the form needs to see the
 * inputs; the img is the same node both with and without JS, so no
 * rebuild is needed).
 *
 * @attr data-focal-x  Initial X coordinate (0–1, default 0.5).
 * @attr data-focal-y  Initial Y coordinate (0–1, default 0.5).
 *
 * @example
 * <crap-focal-point data-focal-x="0.5" data-focal-y="0.5">
 *   <img src="/uploads/photo.jpg" alt="" />
 *   <input type="hidden" name="focal_x" value="0.5000" />
 *   <input type="hidden" name="focal_y" value="0.5000" />
 * </crap-focal-point>
 *
 * @module focal-point
 */

import { css } from './css.js';
import { h } from './h.js';
import { t } from './i18n.js';

const sheet = css`
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

  ::slotted(img) {
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
`;

/** Default focal coordinate (centre). */
const DEFAULT_FOCAL = 0.5;

/**
 * Clamp `n` to the closed `[0, 1]` interval.
 * @param {number} n
 */
function clamp01(n) {
  return Math.max(0, Math.min(1, n));
}

class CrapFocalPoint extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;

    const root = this.attachShadow({ mode: 'open' });
    root.adoptedStyleSheets = [sheet];

    /** @type {HTMLDivElement} */
    this._marker = h('div', { class: 'marker' });
    root.append(
      h('div', { class: 'focal-point' }, h('slot'), this._marker),
      h('p', { class: 'hint', text: t('focal_point_hint') }),
    );
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    const img = /** @type {HTMLImageElement|null} */ (this.querySelector('img'));
    if (!img) return;
    const inputX = /** @type {HTMLInputElement|null} */ (
      this.querySelector('input[name="focal_x"]')
    );
    const inputY = /** @type {HTMLInputElement|null} */ (
      this.querySelector('input[name="focal_y"]')
    );

    const setMarker = (/** @type {number} */ x, /** @type {number} */ y) => {
      this._marker.style.left = `${x * 100}%`;
      this._marker.style.top = `${y * 100}%`;
      if (inputX) inputX.value = x.toFixed(4);
      if (inputY) inputY.value = y.toFixed(4);
    };

    setMarker(this._initialFocal('focalX'), this._initialFocal('focalY'));

    img.addEventListener('click', (e) => {
      const rect = img.getBoundingClientRect();
      setMarker(
        clamp01((e.clientX - rect.left) / rect.width),
        clamp01((e.clientY - rect.top) / rect.height),
      );
    });
  }

  /**
   * Initial focal value for `key` (`'focalX'`/`'focalY'`), falling back
   * to {@link DEFAULT_FOCAL} for missing or non-numeric dataset values.
   *
   * @param {'focalX'|'focalY'} key
   */
  _initialFocal(key) {
    const raw = Number.parseFloat(this.dataset[key] || '');
    return Number.isNaN(raw) ? DEFAULT_FOCAL : raw;
  }
}

customElements.define('crap-focal-point', CrapFocalPoint);
