/**
 * Password visibility toggle — `<crap-password-toggle>`.
 *
 * Light-DOM wrapper around a password `<input>` and a toggle `<button>`.
 * Clicking the button flips the input between `password` and `text` and
 * swaps the slotted Material-Symbols icon glyph.
 *
 * Replaces per-button `onclick=` attributes that would otherwise be
 * blocked by our nonce-based `script-src` CSP.
 *
 * Required slotted markup:
 *   <crap-password-toggle class="form__password-wrapper">
 *     <input type="password" ... />
 *     <button type="button" class="form__password-toggle">
 *       <span>visibility</span>
 *     </button>
 *   </crap-password-toggle>
 *
 * @module password-toggle
 */

const ICON_HIDDEN = 'visibility';
const ICON_VISIBLE = 'visibility_off';

class CrapPasswordToggle extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {HTMLInputElement|null} */
    this._input = null;
    /** @type {((e: Event) => void)|null} */
    this._onClick = null;
  }

  connectedCallback() {
    if (this._connected) return;
    this._input = /** @type {HTMLInputElement|null} */ (this.querySelector('input'));
    if (!this._input) return;
    this._connected = true;

    this._onClick = (e) => this._onToggle(e);
    this.addEventListener('click', this._onClick);
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    if (this._onClick) this.removeEventListener('click', this._onClick);
    this._input = null;
    this._onClick = null;
  }

  /** @param {Event} e */
  _onToggle(e) {
    if (!(e.target instanceof Element)) return;
    const button = /** @type {HTMLElement|null} */ (e.target.closest('.form__password-toggle'));
    if (!button || !this.contains(button) || !this._input) return;

    const reveal = this._input.type === 'password';
    this._input.type = reveal ? 'text' : 'password';
    const icon = button.querySelector('span');
    if (icon) icon.textContent = reveal ? ICON_VISIBLE : ICON_HIDDEN;
  }
}

customElements.define('crap-password-toggle', CrapPasswordToggle);
