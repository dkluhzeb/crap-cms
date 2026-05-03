/**
 * Array/blocks row wrapper — `<crap-array-row>`.
 *
 * Lightweight custom element that wraps a single row inside a
 * `<crap-array-field>`. Today it owns one concern — the row-label
 * watcher that mirrors a configured sub-field's value into the row's
 * collapsible header — but the element exists primarily as a stable
 * boundary that future refactors can hang behaviour on (drag handle
 * scoping, per-row state, custom row decorators).
 *
 * **Light DOM** — keeps the existing `.form__array-row` class for
 * CSS compatibility and lets the parent orchestrator's event
 * delegation continue to find the row's buttons via
 * `closest('crap-array-row')` or the legacy `.form__array-row`.
 *
 * @attr data-row-index The row's current position (0-based). The
 *   parent `<crap-array-field>` rewrites this on add/remove/reorder;
 *   templates must emit it.
 *
 * @example
 * <crap-array-row class="form__array-row" data-row-index="0">
 *   <header>…</header>
 *   <div class="form__array-row-body">…</div>
 * </crap-array-row>
 *
 * @module array-row
 * @stability stable
 */

class CrapArrayRow extends HTMLElement {
  constructor() {
    super();
    /** @type {boolean} */
    this._connected = false;
  }

  connectedCallback() {
    if (this._connected) return;
    this._connected = true;
    this._setupLabelWatcher();
  }

  /**
   * Mirror a configured input's value into the row title element. The
   * label-field name comes from either:
   *   - the surrounding `<template data-label-field="…">` ancestor
   *     (block-level override, set by `<crap-array-field>` when adding
   *     a block row), or
   *   - the parent `<crap-array-field>`'s fieldset `data-label-field`
   *     attribute (uniform-array case).
   *
   * Idempotent — `dataset.labelInit` records that wiring has run, so
   * a row that's reconnected (e.g. via drag-and-drop reorder) doesn't
   * accumulate duplicate listeners.
   */
  _setupLabelWatcher() {
    if (this.dataset.labelInit) return;

    const labelFieldName = this._resolveLabelFieldName();
    if (!labelFieldName) return;

    const titleEl = this.querySelector('.form__array-row-title');
    if (!titleEl) return;

    const suffix = `[${labelFieldName}]`;
    for (const input of /** @type {NodeListOf<HTMLInputElement>} */ (
      this.querySelectorAll('input, select, textarea')
    )) {
      if (!input.name?.endsWith(suffix)) continue;
      input.addEventListener('input', () => {
        if (input.value) titleEl.textContent = input.value;
      });
      this.dataset.labelInit = '1';
      return;
    }
  }

  /**
   * Resolve the configured label-field name for this row, walking up
   * to find either a wrapping `<template data-label-field="…">` (block
   * row context, where the parent field carries multiple block-types
   * each with its own label field) or the parent `<crap-array-field>`'s
   * fieldset.
   *
   * @returns {string|null}
   */
  _resolveLabelFieldName() {
    // Block-row case: the row was cloned from a `<template>` whose
    // `data-label-field` carries the block's specific label field.
    // After clone+insertion the `<template>` is gone — that lookup
    // happens before connectedCallback, so the row author (the
    // orchestrator) propagates the field by setting it as an attribute
    // on the row element itself.
    const direct = this.getAttribute('data-label-field');
    if (direct) return direct;

    // Uniform-array case: pick up the parent fieldset's attribute.
    const fs = this.closest('.form__array');
    return fs?.getAttribute('data-label-field') ?? null;
  }
}

if (!customElements.get('crap-array-row')) {
  customElements.define('crap-array-row', CrapArrayRow);
}
