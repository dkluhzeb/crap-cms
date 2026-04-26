/**
 * Password visibility toggle — `<crap-password-toggle>`.
 *
 * Wraps a slotted password `<input>` (which stays in light DOM so the
 * browser submits it with the surrounding form) and renders its own
 * toggle button + icon in the shadow root. Consumers don't have to
 * remember the magic button structure or wrapper class:
 *
 *   <crap-password-toggle>
 *     <input type="password" name="password" />
 *   </crap-password-toggle>
 *
 * The component sets the slotted input's right-side padding via
 * `::slotted(input)` so the toggle button doesn't overlap the text.
 *
 * @module password-toggle
 */

import { css } from './css.js';
import { h } from './h.js';

const ICON_HIDDEN = 'visibility';
const ICON_VISIBLE = 'visibility_off';

const sheet = css`
  :host {
    display: block;
    position: relative;
  }
  ::slotted(input) {
    padding-right: var(--padding-with-icon);
  }
  .toggle {
    all: unset;
    position: absolute;
    right: var(--space-sm);
    top: 50%;
    transform: translateY(-50%);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    color: var(--text-tertiary);
    width: var(--button-height-sm);
    height: var(--button-height-sm);
    border-radius: var(--radius-sm);
  }
  .toggle:hover {
    color: var(--text-secondary);
    background: var(--bg-hover);
  }
  /* The .material-symbols-outlined class lives in a document-level
     stylesheet (loaded from Google Fonts in the page head) and does not
     pierce the shadow boundary. Re-declare the icon-font properties
     here so the glyph renders instead of the literal ligature text. */
  .toggle .material-symbols-outlined {
    font-family: "Material Symbols Outlined";
    font-weight: normal;
    font-style: normal;
    font-size: var(--icon-md);
    line-height: 1;
    letter-spacing: normal;
    text-transform: none;
    display: inline-block;
    white-space: nowrap;
    word-wrap: normal;
    direction: ltr;
    -webkit-font-feature-settings: "liga";
    font-feature-settings: "liga";
    -webkit-font-smoothing: antialiased;
  }
`;

class CrapPasswordToggle extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    this.shadowRoot.adoptedStyleSheets = [sheet];

    this._icon = h('span', { class: 'material-symbols-outlined', text: ICON_HIDDEN });
    this._button = h(
      'button',
      {
        type: 'button',
        class: 'toggle',
        'aria-label': 'Toggle password visibility',
        onClick: () => this._toggle(),
      },
      this._icon,
    );

    this.shadowRoot.append(h('slot'), this._button);
  }

  _toggle() {
    const input = /** @type {HTMLInputElement|null} */ (this.querySelector('input'));
    if (!input) return;
    const reveal = input.type === 'password';
    input.type = reveal ? 'text' : 'password';
    this._icon.textContent = reveal ? ICON_VISIBLE : ICON_HIDDEN;
  }
}

customElements.define('crap-password-toggle', CrapPasswordToggle);
