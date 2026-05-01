/**
 * Column picker — `<crap-column-picker>`.
 *
 * Renders a checkbox list of column options into itself, packs the
 * user's selection into a single comma-joined `columns` field, and
 * submits via htmx. Reloads on success so the list view picks up the
 * new column selection.
 *
 * Mounted dynamically by `<crap-list-settings>` inside the page-singleton
 * `<crap-drawer>` body. The orchestrator constructs the element with
 * the data already attached:
 *
 * @example
 *   const picker = document.createElement('crap-column-picker');
 *   picker.dataset.collection = 'posts';
 *   picker.dataset.options = JSON.stringify(options);  // ColumnOption[]
 *   drawer.body.appendChild(picker);
 *
 * @attr data-collection  Collection slug — `hx-post` target.
 * @attr data-options     JSON-encoded `ColumnOption[]` list.
 *
 * Override pattern: drop a replacement at
 * `<config_dir>/static/components/list-settings/column-picker.js` for
 * a full replace, or subclass `CrapColumnPicker` (re-exported) for
 * incremental customization.
 *
 * @module list-settings/column-picker
 * @stability stable
 */

import { clear, h } from '../_internal/h.js';
import { t } from '../_internal/i18n.js';

/** @typedef {{ key: string, label: string, selected: boolean }} ColumnOption */

export class CrapColumnPicker extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
    /** @type {((evt: Event) => void)|null} */
    this._onAfterRequest = null;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    const slug = this.dataset.collection;
    if (!slug) return;
    const options = this._readOptions();

    const form = this._buildForm(options, slug);
    clear(this);
    this.appendChild(form);

    // htmx auto-discovery doesn't traverse shadow roots; the picker
    // mounts inside `<crap-drawer>`'s shadow DOM body, so the new
    // form's `hx-*` attributes are invisible to htmx until we tell it
    // where to look. Without this, submit goes through the browser's
    // default form action (= the page URL).
    if (typeof htmx !== 'undefined') htmx.process(form);
  }

  disconnectedCallback() {
    if (!this._connected) return;
    this._connected = false;
    // The form is removed with the host; its `htmx:afterRequest` listener
    // is implicit-cleaned via GC. No explicit removeEventListener needed.
  }

  /**
   * Read the JSON-encoded option list from `data-options`. Returns an
   * empty array if the attribute is missing or unparseable.
   *
   * @returns {ColumnOption[]}
   */
  _readOptions() {
    const raw = this.getAttribute('data-options');
    if (!raw) return [];
    try {
      const parsed = JSON.parse(raw);
      return Array.isArray(parsed) ? parsed : [];
    } catch {
      return [];
    }
  }

  /**
   * Build the picker form. Submission goes through htmx: `hx-post` on
   * form submit, urlencoded body auto-built from form fields, CSRF added
   * by the `htmx:configRequest` listener in `templates/layout/base.hbs`,
   * `hx-swap="none"` because we just need the success status (the page
   * reloads to pick up the new selection from the server).
   *
   * Checked column keys assemble into a single hidden `columns` field —
   * the server endpoint expects a comma-joined list, not duplicate
   * `column=` params from a `<select multiple>`.
   *
   * @param {ColumnOption[]} options
   * @param {string} slug
   * @returns {HTMLFormElement}
   */
  _buildForm(options, slug) {
    const list = h(
      'div',
      { class: 'column-picker__list' },
      options.map((opt) =>
        h(
          'label',
          { class: 'column-picker__item' },
          h('input', {
            type: 'checkbox',
            class: 'column-picker__checkbox',
            value: opt.key,
            checked: opt.selected,
          }),
          h('span', { text: t(opt.label) }),
        ),
      ),
    );
    const columnsInput = h('input', { type: 'hidden', name: 'columns', value: '' });
    const updateColumns = () => {
      const checked = /** @type {NodeListOf<HTMLInputElement>} */ (
        list.querySelectorAll('input[type="checkbox"]:checked')
      );
      columnsInput.value = [...checked].map((cb) => cb.value).join(',');
    };
    list.addEventListener('change', updateColumns);
    updateColumns();

    const form = /** @type {HTMLFormElement} */ (
      h(
        'form',
        {
          class: 'column-picker',
          'hx-post': `/admin/api/user-settings/${slug}`,
          'hx-swap': 'none',
        },
        columnsInput,
        list,
        h(
          'div',
          { class: 'column-picker__footer' },
          h('button', {
            type: 'submit',
            class: ['button', 'button--primary', 'button--small'],
            text: t('save'),
          }),
        ),
      )
    );

    // Listen on the form. htmx events bubble with `composed: true`,
    // so the form node sees them even though it lives inside
    // `<crap-drawer>`'s shadow root.
    this._onAfterRequest = (evt) => {
      const detail = /** @type {any} */ (evt).detail;
      if (!detail?.successful) return;
      this._onSuccess();
    };
    form.addEventListener('htmx:afterRequest', this._onAfterRequest);

    return form;
  }

  /**
   * Hook subclasses can override to customize the post-submit flow.
   * Default: dispatch a `crap:column-picker-saved` event the
   * orchestrator listens for, then full-page reload as a fallback.
   *
   * Subclasses overriding this should still trigger drawer-close +
   * page-refresh equivalent, or the user's choice will appear lost.
   */
  _onSuccess() {
    // Emit a bubbling event so the surrounding orchestrator can close
    // the drawer / refresh / etc. on its own terms. Plus the
    // window.location.reload() guarantees the new column selection is
    // visible even if no listener picks the event up.
    this.dispatchEvent(
      new CustomEvent('crap:column-picker-saved', { bubbles: true, composed: true }),
    );
    window.location.reload();
  }
}

if (!customElements.get('crap-column-picker')) {
  customElements.define('crap-column-picker', CrapColumnPicker);
}
