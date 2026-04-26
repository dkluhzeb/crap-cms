/**
 * Tagged-template helper for constructable stylesheets.
 *
 * Replaces the `new CSSStyleSheet() + replaceSync(\`...\`)` boilerplate
 * with a single tagged template that returns a ready-to-adopt sheet:
 *
 * @example
 * import { css } from './css.js';
 * const sheet = css`
 *   :host { display: contents; }
 *   ul { margin: 0; }
 * `;
 * shadowRoot.adoptedStyleSheets = [sheet];
 *
 * Naming matches Lit / Stencil / FAST / WebReflection's `css` tag, so
 * editor extensions that highlight `css\`…\`` template literals
 * (lit-plugin, vscode-styled-components, etc.) work out of the box.
 *
 * @module css
 */

/**
 * Build a `CSSStyleSheet` from a template literal. Interpolations are
 * coerced to strings via `String.raw` so escapes inside CSS (`\1` etc.)
 * are preserved.
 *
 * @param {TemplateStringsArray} strings
 * @param {...unknown} values
 * @returns {CSSStyleSheet}
 */
export function css(strings, ...values) {
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(String.raw({ raw: strings }, ...values));
  return sheet;
}
