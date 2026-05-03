/**
 * Star-rating input — `<crap-stars>`.
 *
 * A form-associated custom element that renders 5 click/keyboard-driven
 * stars and submits its `value` attribute with the surrounding form.
 *
 * Usage in a Handlebars template:
 *
 *   <crap-stars
 *     name="rating"
 *     value="3"
 *     max="5"
 *     required
 *   ></crap-stars>
 *
 * Why form-associated custom elements: implementing
 * `static formAssociated = true` plus `attachInternals()` /
 * `setFormValue(...)` lets a custom element participate in native form
 * submission and validation the same way `<input>` does — no hidden
 * input shim, no manual FormData injection. The form serializer reads
 * the component as `name=value` automatically.
 *
 * Lives in `<config_dir>/static/components/` and is loaded via the
 * `body_end_scripts` slot (`templates/slots/body_end_scripts/rating.hbs`).
 *
 * @module crap-stars
 */

const STAR_COUNT_DEFAULT = 5;

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
  :host {
    display: inline-flex;
    align-items: center;
    gap: 0.125rem;
    --star-color: var(--text-tertiary, #888);
    --star-color-active: var(--color-primary, #f5a623);
    --star-color-hover: var(--color-primary, #f5a623);
    font-size: 1.5rem;
    line-height: 1;
  }
  :host([data-error]) {
    outline: 1px solid var(--color-error, #c33);
    outline-offset: 2px;
    border-radius: var(--radius-sm, 4px);
  }
  button {
    all: unset;
    cursor: pointer;
    color: var(--star-color);
    padding: 0.125rem;
    transition: color 120ms ease;
    line-height: 1;
  }
  button:focus-visible {
    outline: 2px solid var(--color-primary, #07f);
    outline-offset: 1px;
    border-radius: var(--radius-sm, 4px);
  }
  button[aria-pressed="true"] {
    color: var(--star-color-active);
  }
  :host(:not([readonly])) button:hover,
  :host(:not([readonly])) button:hover ~ button {
    /* Hover preview: light up everything up to the hovered star, dim
       the rest. The CSS sibling selector lets us colour purely from
       hover state without JS bookkeeping. */
  }
  :host(:not([readonly])):has(button:hover) button {
    color: var(--star-color);
  }
  :host(:not([readonly])):has(button:hover) button:hover,
  :host(:not([readonly])):has(button:hover) button:hover ~ button:not(:hover) {
    color: var(--star-color);
  }
  :host(:not([readonly])) button:hover,
  :host(:not([readonly])) button:has(~ button:hover) {
    color: var(--star-color-hover);
  }
  :host([readonly]) button {
    cursor: default;
    pointer-events: none;
  }
`);

class CrapStars extends HTMLElement {
  static formAssociated = true;
  static observedAttributes = ['value', 'max', 'readonly'];

  constructor() {
    super();
    this._internals = this.attachInternals();
    // `delegatesFocus` makes a click on a <label for="..."> targeting
    // this host land on the first focusable element inside the shadow
    // root (the first star button), so the label-association the
    // form's `partials/field` wrapper sets up actually works.
    this.attachShadow({ mode: 'open', delegatesFocus: true });
    this.shadowRoot.adoptedStyleSheets = [sheet];

    this._buttons = [];
    this._value = 0;
  }

  connectedCallback() {
    this._render();
    this._sync();
  }

  attributeChangedCallback(name, _oldValue, newValue) {
    if (name === 'value') {
      this._value = clampInt(newValue, 0, this._max());
      this._sync();
    } else if (name === 'max') {
      this._render();
      this._sync();
    } else if (name === 'readonly') {
      // No re-render needed — CSS handles the readonly state.
    }
  }

  /** @returns {number} */
  get value() {
    return this._value;
  }
  set value(v) {
    this.setAttribute('value', String(v));
  }

  _max() {
    const raw = parseInt(this.getAttribute('max') ?? '', 10);
    return Number.isFinite(raw) && raw > 0 ? raw : STAR_COUNT_DEFAULT;
  }

  _render() {
    this.shadowRoot.replaceChildren();
    this._buttons = [];

    const max = this._max();
    for (let i = 1; i <= max; i++) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.setAttribute('aria-label', `${i} star${i === 1 ? '' : 's'}`);
      btn.dataset.value = String(i);
      btn.textContent = '★'; // ★
      btn.addEventListener('click', () => this._set(i));
      btn.addEventListener('keydown', (e) => this._onKeydown(e, i));
      this._buttons.push(btn);
      this.shadowRoot.append(btn);
    }
  }

  _onKeydown(e, current) {
    const max = this._max();
    if (e.key === 'ArrowRight' || e.key === 'ArrowUp') {
      e.preventDefault();
      this._set(Math.min(current + 1, max));
      this._buttons[Math.min(current, max - 1)]?.focus();
    } else if (e.key === 'ArrowLeft' || e.key === 'ArrowDown') {
      e.preventDefault();
      this._set(Math.max(current - 1, 0));
      this._buttons[Math.max(current - 2, 0)]?.focus();
    } else if (e.key === '0' || e.key === 'Backspace' || e.key === 'Delete') {
      e.preventDefault();
      this._set(0);
    }
  }

  _set(v) {
    if (this.hasAttribute('readonly')) return;
    if (v === this._value) return;
    this._value = v;
    this.setAttribute('value', String(v));
    this.dispatchEvent(new Event('change', { bubbles: true }));
  }

  _sync() {
    // Update aria-pressed state on each star + sync the form value.
    for (const btn of this._buttons) {
      const i = Number(btn.dataset.value);
      btn.setAttribute('aria-pressed', i <= this._value ? 'true' : 'false');
    }
    this._internals.setFormValue(this._value > 0 ? String(this._value) : null);
    if (this.hasAttribute('required') && this._value < 1) {
      this._internals.setValidity({ valueMissing: true }, 'Please select a rating.');
    } else {
      this._internals.setValidity({});
    }
  }
}

function clampInt(raw, lo, hi) {
  const n = parseInt(raw ?? '', 10);
  if (!Number.isFinite(n)) return 0;
  return Math.max(lo, Math.min(hi, n));
}

if (!customElements.get('crap-stars')) {
  customElements.define('crap-stars', CrapStars);
}
