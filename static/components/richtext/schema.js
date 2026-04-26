/**
 * ProseMirror schema construction for `<crap-richtext>`.
 *
 * Pure functions of `(PM, has, customNodes)`. No `this` dependency,
 * no DOM access — testable in isolation given a PM stub.
 *
 * @module richtext/schema
 */

/**
 * @typedef {(name: string) => boolean} FeatureCheck
 *
 * @typedef {{
 *   name: string,
 *   label: string,
 *   inline?: boolean,
 *   attrs?: Array<{ name: string, default?: any }>,
 * }} CustomNodeDef
 */

/**
 * Build a fresh ProseMirror `Schema` for the editor.
 *
 *  - List nodes are added when either list feature is enabled.
 *  - Block nodes are removed for disabled features (`heading`,
 *    `codeBlock`, `blockquote`, `horizontalRule`).
 *  - Custom nodes are appended as atomic block/inline nodes.
 *
 * @param {any} PM ProseMirror namespace (`window.ProseMirror`).
 * @param {FeatureCheck} has
 * @param {CustomNodeDef[]} customNodes
 */
export function buildSchema(PM, has, customNodes) {
  let nodes = PM.basicSchema.spec.nodes;
  if (has('orderedList') || has('bulletList')) {
    nodes = PM.addListNodes(nodes, 'paragraph block*', 'block');
  }
  if (!has('heading')) nodes = nodes.remove('heading');
  if (!has('codeBlock')) nodes = nodes.remove('code_block');
  if (!has('blockquote')) nodes = nodes.remove('blockquote');
  if (!has('horizontalRule')) nodes = nodes.remove('horizontal_rule');

  for (const def of customNodes) {
    nodes = nodes.addToEnd(def.name, buildCustomNodeSpec(def));
  }

  return new PM.Schema({ nodes, marks: buildMarks(PM, has) });
}

/**
 * @param {any} PM
 * @param {FeatureCheck} has
 */
function buildMarks(PM, has) {
  const baseMarks = PM.basicSchema.spec.marks;
  /** @type {Record<string, any>} */
  const marks = {};
  if (has('bold') && baseMarks.get('strong')) marks.strong = baseMarks.get('strong');
  if (has('italic') && baseMarks.get('em')) marks.em = baseMarks.get('em');
  if (has('code') && baseMarks.get('code')) marks.code = baseMarks.get('code');
  if (has('link')) marks.link = buildLinkMarkSpec();
  return marks;
}

function buildLinkMarkSpec() {
  return {
    attrs: {
      href: { default: '' },
      title: { default: null },
      target: { default: null },
      rel: { default: null },
    },
    inclusive: false,
    parseDOM: [
      {
        tag: 'a[href]',
        getAttrs: (/** @type {Element} */ dom) => ({
          href: dom.getAttribute('href'),
          title: dom.getAttribute('title'),
          target: dom.getAttribute('target'),
          rel: dom.getAttribute('rel'),
        }),
      },
    ],
    toDOM: (/** @type {any} */ node) => {
      const { href, title, target, rel } = node.attrs;
      /** @type {Record<string, string>} */
      const attrs = { href };
      if (title) attrs.title = title;
      if (target) attrs.target = target;
      if (rel) attrs.rel = rel;
      return ['a', attrs, 0];
    },
  };
}

/** @param {CustomNodeDef} def */
function buildCustomNodeSpec(def) {
  return {
    group: def.inline ? 'inline' : 'block',
    inline: def.inline,
    atom: true,
    attrs: Object.fromEntries((def.attrs || []).map((a) => [a.name, { default: a.default ?? '' }])),
    toDOM: (/** @type {any} */ node) => [
      'crap-node',
      {
        'data-type': def.name,
        'data-attrs': JSON.stringify(node.attrs),
      },
    ],
    parseDOM: [
      {
        tag: `crap-node[data-type="${def.name}"]`,
        getAttrs: (/** @type {Element} */ dom) => {
          try {
            return JSON.parse(dom.getAttribute('data-attrs') || '{}');
          } catch {
            return {};
          }
        },
      },
    ],
  };
}
