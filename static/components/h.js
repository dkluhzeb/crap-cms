/**
 * Tiny DOM-construction helper. Replaces `innerHTML` template literals with
 * type-safe `createElement` calls. No runtime, no framework.
 *
 * Convention: hyperscript-shape `h(tag, props, ...children)`. Same signature
 * as Preact / Vue 3 `h` / Mithril `m` etc. — every JS dev recognises it.
 *
 * Why not `innerHTML`: a CMS handles user-contributed content. Inline-HTML
 * interpolation requires "did I remember to escape?" discipline at every
 * call site; with `h()`, HTML injection is unwriteable at the API level
 * (props go through `setAttribute` / `textContent`, never an HTML parser).
 *
 * @module h
 */

/**
 * @typedef {Node | string | number | false | null | undefined} HChild
 * @typedef {HChild | HChild[]} HChildren
 */

/**
 * Recognised shorthand props applied before native attributes / properties.
 *
 * @typedef {Object} HShorthands
 * @property {string | (string|false|null|undefined)[]} [class]
 *   Token list applied via `classList.add(...)`. Falsy tokens are dropped, so
 *   `class: ['base', cond && 'is-active']` works inline.
 * @property {string} [text]
 *   Assigned to `textContent`. When set, any positional `children` are
 *   ignored — use one or the other, not both.
 * @property {Record<string,string>} [dataset]
 *   Merged into `el.dataset` (each entry becomes a `data-*` attribute).
 * @property {Partial<CSSStyleDeclaration>} [style]
 *   Merged into `el.style` (programmatic — CSP-exempt).
 */

/**
 * Full props bag for `h()`. Intersects shorthands with the per-tag element
 * property type (so `h('a', { href })` narrows correctly and typos like
 * `h('button', { hreff })` are flagged), plus a free-form record escape hatch
 * for `aria-*` / `data-*` / custom-element attributes that aren't on the
 * standard `HTMLElementTagNameMap` entries.
 *
 * @template {keyof HTMLElementTagNameMap} K
 * @typedef {HShorthands
 *   & Partial<HTMLElementTagNameMap[K]>
 *   & Record<string, any>} HProps
 */

/**
 * Build an element with type-narrowed return.
 *
 * Recognised props (in order of precedence):
 *   - `class`  — string | (string|falsy)[] → classList.add(...tokens)
 *   - `text`   — string → textContent (suppresses positional children)
 *   - `dataset` — Record<string,string>
 *   - `style`  — Partial<CSSStyleDeclaration>
 *   - `on<Event>` — function → addEventListener('event', fn)
 *   - boolean true → setAttribute(k, '')
 *   - boolean false / null / undefined → skipped
 *   - any other key → setAttribute(k, String(v))
 *
 * Children: `Node` is appended; strings/numbers become text nodes; nested
 * arrays are flattened one level so `items.map(h(...))` works inline; falsy
 * children (`null`/`false`/`undefined`) are dropped so `cond && h(...)` works.
 *
 * @template {keyof HTMLElementTagNameMap} K
 * @param {K} tag
 * @param {HProps<K> | null} [props]
 * @param {...HChildren} children
 * @returns {HTMLElementTagNameMap[K]}
 */
export function h(tag, props, ...children) {
  const el = document.createElement(tag);
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v == null || v === false) continue;
      if (k === 'class') {
        const tokens = Array.isArray(v) ? v : String(v).split(/\s+/);
        for (const t of tokens) if (t) el.classList.add(t);
      } else if (k === 'text') {
        el.textContent = String(v);
      } else if (k === 'dataset') {
        Object.assign(el.dataset, v);
      } else if (k === 'style' && typeof v === 'object') {
        Object.assign(el.style, v);
      } else if (k.startsWith('on') && typeof v === 'function') {
        el.addEventListener(k.slice(2).toLowerCase(), v);
      } else if (typeof v === 'boolean') {
        if (v) el.setAttribute(k, '');
      } else {
        el.setAttribute(k, String(v));
      }
    }
  }
  if (!props || props.text === undefined) {
    for (const c of children.flat()) {
      if (c == null || c === false) continue;
      el.append(c instanceof Node ? c : String(c));
    }
  }
  return el;
}

/**
 * Clear an element's children. Wraps `replaceChildren()` for grep-ability —
 * `clear(body)` is greppable; `body.replaceChildren()` is harder to spot.
 *
 * @param {Element|DocumentFragment|ShadowRoot} node
 */
export function clear(node) {
  node.replaceChildren();
}
