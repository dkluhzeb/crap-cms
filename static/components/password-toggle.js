/**
 * Password visibility toggle — `<crap-password-toggle>`.
 *
 * Light-DOM wrapper around a password input and a toggle button. Clicking
 * the button flips the input between `password` and `text` and swaps the
 * material-symbols icon between `visibility` and `visibility_off`.
 *
 * Replaces per-button `onclick=` attributes so the admin UI works under a
 * nonce-based Content-Security-Policy (inline event handlers are blocked
 * when `script-src` uses `'nonce-...'` without `'unsafe-hashes'`).
 *
 * Markup contract:
 *   <crap-password-toggle class="form__password-wrapper">
 *     <input type="password" ... />
 *     <button type="button" class="form__password-toggle" ...>
 *       <span>visibility</span>
 *     </button>
 *   </crap-password-toggle>
 *
 * @module password-toggle
 */

class CrapPasswordToggle extends HTMLElement {
  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    this._onClick = (event) => {
      const button = /** @type {HTMLElement} */ (event.target).closest(
        '.form__password-toggle',
      );

      if (!button || !this.contains(button)) return;

      const input = /** @type {HTMLInputElement|null} */ (
        this.querySelector('input')
      );

      if (!input) return;

      const wasPassword = input.type === 'password';

      input.type = wasPassword ? 'text' : 'password';

      const icon = button.querySelector('span');

      if (icon) {
        icon.textContent = wasPassword ? 'visibility_off' : 'visibility';
      }
    };

    this.addEventListener('click', this._onClick);
  }

  disconnectedCallback() {
    if (this._onClick) {
      this.removeEventListener('click', this._onClick);
    }
  }
}

customElements.define('crap-password-toggle', CrapPasswordToggle);
