/**
 * Shared base class for the three dropdown-pickers
 * (`<crap-locale-picker>`, `<crap-ui-locale-picker>`, `<crap-theme-picker>`).
 *
 * Each subclass declares its toggle/dropdown/item selectors, the open-class
 * applied to the dropdown, and the dataset key holding the option's value.
 * The base wires the toggle/select/outside-click listeners and dispatches
 * to `_onValue(value)` when an option is selected. Optional
 * `_afterToggle()` hook lets subclasses refresh per-toggle state (e.g.
 * highlighting the active option).
 *
 * @module picker-base
 * @stability internal
 */

/**
 * @abstract
 */
export class CrapPickerBase extends HTMLElement {
  /** @type {string} Selector for the toggle button inside the host. */
  static toggleSelector = '';
  /** @type {string} Selector for the dropdown container inside the host. */
  static dropdownSelector = '';
  /** @type {string} Selector for individual option items inside the dropdown. */
  static itemSelector = '';
  /** @type {string} Class added to the dropdown when open. */
  static openClass = '';
  /** @type {string} `dataset` key on the option holding its value (camelCase). */
  static valueDatasetKey = '';

  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {HTMLElement|null} */
    this._toggle = null;
    /** @type {HTMLElement|null} */
    this._dropdown = null;
    /** @type {((e: Event) => void)|null} */
    this._onToggle = null;
    /** @type {((e: Event) => void)|null} */
    this._onSelect = null;
    /** @type {((e: Event) => void)|null} */
    this._onOutsideClick = null;
  }

  connectedCallback() {
    if (this._connected) return;
    const cls = /** @type {typeof CrapPickerBase} */ (this.constructor);
    this._toggle = /** @type {HTMLElement|null} */ (this.querySelector(cls.toggleSelector));
    this._dropdown = /** @type {HTMLElement|null} */ (this.querySelector(cls.dropdownSelector));
    if (!this._toggle || !this._dropdown) return;
    this._connected = true;

    this._onToggle = (e) => {
      e.stopPropagation();
      this._dropdown?.classList.toggle(cls.openClass);
      this._afterToggle();
    };

    this._onSelect = (e) => {
      if (!(e.target instanceof Element)) return;
      const btn = /** @type {HTMLElement|null} */ (e.target.closest(cls.itemSelector));
      if (!btn) return;
      const value = btn.dataset[cls.valueDatasetKey];
      if (value === undefined) return;
      this._dropdown?.classList.remove(cls.openClass);
      this._onValue(value);
    };

    this._onOutsideClick = (e) => {
      if (!(e.target instanceof Node)) return;
      if (!this.contains(e.target)) {
        this._dropdown?.classList.remove(cls.openClass);
      }
    };

    this._toggle.addEventListener('click', this._onToggle);
    this._dropdown.addEventListener('click', this._onSelect);
    document.addEventListener('click', this._onOutsideClick);
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    if (this._toggle && this._onToggle) this._toggle.removeEventListener('click', this._onToggle);
    if (this._dropdown && this._onSelect)
      this._dropdown.removeEventListener('click', this._onSelect);
    if (this._onOutsideClick) document.removeEventListener('click', this._onOutsideClick);
    this._toggle = null;
    this._dropdown = null;
    this._onToggle = null;
    this._onSelect = null;
    this._onOutsideClick = null;
  }

  /**
   * Subclasses override to act on the chosen value (set cookie + reload,
   * POST + reload, persist + apply, etc.).
   *
   * @param {string} value
   * @abstract
   */
  // eslint-disable-next-line no-unused-vars
  _onValue(_value) {
    throw new Error('CrapPickerBase subclass must implement _onValue(value).');
  }

  /** Optional hook called after every toggle. Default: no-op. */
  _afterToggle() {}
}
