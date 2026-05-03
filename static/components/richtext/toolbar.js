/**
 * Toolbar construction + active-state logic for `<crap-richtext>`.
 *
 * Two halves:
 *  - {@link buildToolbarNodes} — pure DOM construction from `(has, customNodes)`.
 *  - {@link isCommandActive} / {@link markActive} / {@link getMarkAttrs} —
 *    pure helpers that read PM state to decide which toolbar buttons are lit.
 *
 * @module richtext/toolbar
 * @stability internal
 */

import { h } from '../_internal/h.js';

/**
 * @typedef {(name: string) => boolean} FeatureCheck
 *
 * @typedef {{ name: string, label: string }} CustomNodeRef
 */

/**
 * Build all toolbar group nodes for the configured features.
 *
 * @param {FeatureCheck} has
 * @param {CustomNodeRef[]} customNodes
 * @returns {HTMLElement[]}
 */
export function buildToolbarNodes(has, customNodes) {
  /** @type {HTMLElement[]} */
  const groups = [];
  const inline = inlineGroup(has);
  if (inline) groups.push(inline);
  const block = blockGroup(has);
  if (block) groups.push(block);
  const list = listGroup(has);
  if (list) groups.push(list);
  if (customNodes.length > 0) groups.push(customNodeGroup(customNodes));
  groups.push(historyGroup());
  return groups;
}

/**
 * Whether mark `markType` is active in the current selection.
 *
 * @param {any} state PM EditorState.
 * @param {any} markType PM MarkType.
 */
export function markActive(state, markType) {
  const { from, $from, to, empty } = state.selection;
  if (empty) return !!markType.isInSet(state.storedMarks || $from.marks());
  return state.doc.rangeHasMark(from, to, markType);
}

/**
 * Read the active mark's attrs at the cursor (or `{}` if absent).
 *
 * @param {any} state
 * @param {any} markType
 */
export function getMarkAttrs(state, markType) {
  const marks = state.storedMarks || state.selection.$from.marks();
  const mark = markType.isInSet(marks);
  return mark ? { ...mark.attrs } : {};
}

/**
 * Whether the toolbar button for `cmd` should be in its active state.
 *
 * @param {string} cmd
 * @param {any} state
 * @param {any} schema
 * @param {FeatureCheck} has
 */
export function isCommandActive(cmd, state, schema, has) {
  switch (cmd) {
    case 'bold':
      return !!(has('bold') && schema.marks.strong && markActive(state, schema.marks.strong));
    case 'italic':
      return !!(has('italic') && schema.marks.em && markActive(state, schema.marks.em));
    case 'code':
      return !!(has('code') && schema.marks.code && markActive(state, schema.marks.code));
    case 'link':
      return !!(has('link') && schema.marks.link && markActive(state, schema.marks.link));
    case 'h1':
    case 'h2':
    case 'h3':
      return !!(
        has('heading') &&
        schema.nodes.heading &&
        state.selection.$from.parent.type === schema.nodes.heading &&
        state.selection.$from.parent.attrs.level === Number(cmd[1])
      );
    case 'paragraph':
      return state.selection.$from.parent.type === schema.nodes.paragraph;
    default:
      return false;
  }
}

/* ── Group builders ─────────────────────────────────────────────── */

/**
 * @param {string} cmd
 * @param {string} title
 * @param {Node | string} content
 */
function btn(cmd, title, content) {
  return h('button', { type: 'button', dataset: { cmd }, title }, content);
}

/** @param {HTMLElement[]} buttons */
function group(buttons) {
  return h('div', { class: 'richtext__toolbar-group' }, ...buttons);
}

/** @param {FeatureCheck} has */
function inlineGroup(has) {
  const buttons = [
    has('bold') && btn('bold', 'Bold (Ctrl+B)', h('strong', { text: 'B' })),
    has('italic') && btn('italic', 'Italic (Ctrl+I)', h('em', { text: 'I' })),
    has('code') && btn('code', 'Inline code (Ctrl+`)', h('code', { text: '</>' })),
    has('link') && btn('link', 'Link', 'Link'),
  ].filter(Boolean);
  return buttons.length > 0 ? group(/** @type {HTMLElement[]} */ (buttons)) : null;
}

/** @param {FeatureCheck} has */
function blockGroup(has) {
  if (!has('heading')) return null;
  return group([
    btn('h1', 'Heading 1', 'H1'),
    btn('h2', 'Heading 2', 'H2'),
    btn('h3', 'Heading 3', 'H3'),
    btn('paragraph', 'Paragraph', 'P'),
  ]);
}

/** @param {FeatureCheck} has */
function listGroup(has) {
  const buttons = [
    has('bulletList') && btn('ul', 'Bullet list', 'UL'),
    has('orderedList') && btn('ol', 'Ordered list', 'OL'),
    has('blockquote') && btn('blockquote', 'Blockquote', 'Quote'),
    has('horizontalRule') && btn('hr', 'Horizontal rule', 'HR'),
  ].filter(Boolean);
  return buttons.length > 0 ? group(/** @type {HTMLElement[]} */ (buttons)) : null;
}

/** @param {CustomNodeRef[]} customNodes */
function customNodeGroup(customNodes) {
  return group(customNodes.map((nd) => btn(`insert-${nd.name}`, `Insert ${nd.label}`, nd.label)));
}

function historyGroup() {
  return group([btn('undo', 'Undo (Ctrl+Z)', 'Undo'), btn('redo', 'Redo (Ctrl+Shift+Z)', 'Redo')]);
}
