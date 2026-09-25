/**
 * Field widths — applies an `admin.width` that is not a named preset.
 *
 * `full`, `half` and `third` are styled by the stylesheet. Any other CSS
 * width reaches the page as `data-field-width` on the field's wrapper; the
 * admin's CSP refuses inline `style` attributes, so the width is set here as
 * the wrapper's `--field-basis` instead. A percentage leaves room for the
 * gap between fields, so `50%` twice fills a row just as `half` does; any
 * other length is used as given.
 *
 * Runs over the page's fields at load and over every element added later
 * (HTMX swaps, new array and blocks rows, the create drawer).
 *
 * @module field-width
 * @category enhancer
 * @stability internal
 */

/** Widths the stylesheet lays out by itself. */
const PRESETS = new Set(['full', 'half', 'third']);

const PERCENT = /^(\d+(?:\.\d+)?)%$/;

/**
 * The flex basis for a CSS width: a percentage minus its share of the gap
 * (`--field-gap`) so percentages summing to 100% fill a row exactly; any
 * other value unchanged.
 *
 * @param {string} width
 * @returns {string}
 */
export function fieldBasis(width) {
  const match = PERCENT.exec(width.trim());
  if (!match) return width;

  const gapShare = Number((1 - Number(match[1]) / 100).toFixed(4));
  return `calc(${match[1]}% - var(--field-gap, 0px) * ${gapShare})`;
}

/**
 * Apply the width of every `[data-field-width]` element in `root`, `root`
 * itself included. Named presets are left to the stylesheet.
 *
 * @param {Element|Document|DocumentFragment} root
 */
export function applyFieldWidths(root) {
  const targets = /** @type {HTMLElement[]} */ ([...root.querySelectorAll('[data-field-width]')]);
  if (root instanceof HTMLElement && root.dataset.fieldWidth) targets.push(root);

  for (const el of targets) {
    const width = el.dataset.fieldWidth ?? '';
    if (!width || PRESETS.has(width)) continue;
    el.style.setProperty('--field-basis', fieldBasis(width));
  }
}

applyFieldWidths(document);

new MutationObserver((records) => {
  for (const record of records) {
    for (const node of record.addedNodes) {
      if (node instanceof Element) applyFieldWidths(node);
    }
  }
}).observe(document.documentElement, { childList: true, subtree: true });
