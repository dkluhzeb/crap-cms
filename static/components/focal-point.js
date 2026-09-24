/**
 * Focal point picker — `<crap-focal-point>`.
 *
 * Composes a slotted `<img>` (the preview) with a crosshair marker
 * rendered above it inside the shadow root. Clicking the image — or,
 * with the picker focused, pressing the arrow keys (Shift for a finer
 * step) — moves the marker and writes the normalised `[0, 1]`
 * coordinates into the slotted hidden inputs `focal_x` / `focal_y` so
 * they submit with the surrounding form. Every move dispatches
 * `crap:change`, so the unsaved-changes guard sees it.
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
 * @category form-field
 * @stability stable
 */

import { css } from './_internal/css.js';
import { h } from './_internal/h.js';
import { t } from './_internal/i18n.js';
import { EV_CHANGE } from './events.js';

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

  .focal-point:focus-visible {
    outline: 2px solid var(--color-primary, #6366f1);
    outline-offset: 2px;
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

/** Arrow-key step, as a fraction of the image. */
const KEY_STEP = 0.05;

/** Arrow-key step with Shift held. */
const FINE_KEY_STEP = 0.01;

/** Arrow key → `[dx, dy]` direction. */
const KEY_DIRECTIONS = /** @type {Record<string, [number, number]>} */ ({
  ArrowLeft: [-1, 0],
  ArrowRight: [1, 0],
  ArrowUp: [0, -1],
  ArrowDown: [0, 1],
});

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

    /** @type {number} */
    this._x = DEFAULT_FOCAL;
    /** @type {number} */
    this._y = DEFAULT_FOCAL;

    /** @type {HTMLDivElement} */
    this._marker = h('div', { class: 'marker' });
    /** @type {HTMLDivElement} */
    this._picker = h(
      'div',
      {
        class: 'focal-point',
        tabindex: '0',
        role: 'group',
        'aria-label': t('focal_point_hint'),
      },
      h('slot'),
      this._marker,
    );
    root.append(this._picker, h('p', { class: 'hint', text: t('focal_point_hint') }));
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    const img = /** @type {HTMLImageElement|null} */ (this.querySelector('img'));
    if (!img) return;

    this._render(this._initialFocal('focalX'), this._initialFocal('focalY'));

    img.addEventListener('click', (e) => {
      const rect = img.getBoundingClientRect();
      this._move((e.clientX - rect.left) / rect.width, (e.clientY - rect.top) / rect.height);
    });
    this._picker.addEventListener('keydown', (e) => this._onKeydown(e));
  }

  /**
   * Arrow keys nudge the focal point; Shift makes the step finer.
   *
   * @param {KeyboardEvent} e
   */
  _onKeydown(e) {
    const direction = KEY_DIRECTIONS[e.key];
    if (!direction) return;

    e.preventDefault();
    const step = e.shiftKey ? FINE_KEY_STEP : KEY_STEP;
    this._move(this._x + direction[0] * step, this._y + direction[1] * step);
  }

  /**
   * Move the focal point to `(x, y)` (clamped) as a user edit: render it and
   * announce the change to the surrounding form.
   *
   * @param {number} x
   * @param {number} y
   */
  _move(x, y) {
    this._render(clamp01(x), clamp01(y));
    this.dispatchEvent(new Event(EV_CHANGE, { bubbles: true }));
  }

  /**
   * Place the marker at `(x, y)` and write the coordinates into the hidden
   * inputs.
   *
   * @param {number} x
   * @param {number} y
   */
  _render(x, y) {
    this._x = x;
    this._y = y;
    this._marker.style.left = `${x * 100}%`;
    this._marker.style.top = `${y * 100}%`;

    const inputX = /** @type {HTMLInputElement|null} */ (
      this.querySelector('input[name="focal_x"]')
    );
    const inputY = /** @type {HTMLInputElement|null} */ (
      this.querySelector('input[name="focal_y"]')
    );
    if (inputX) inputX.value = x.toFixed(4);
    if (inputY) inputY.value = y.toFixed(4);
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
