/**
 * ProseMirror NodeView for custom nodes inside `<crap-richtext>`.
 *
 * Renders as a styled card (block) or pill (inline). Double-click
 * delegates to the host component's edit modal.
 *
 * @module richtext/node-view
 */

import { h } from '../h.js';

/**
 * @typedef {{
 *   name: string,
 *   label?: string,
 *   inline?: boolean,
 *   attrs?: Array<{ name: string, [k: string]: any }>,
 * }} CustomNodeDef
 *
 * Host shape required from the surrounding shadow root: an element
 * exposing `_openNodeEditModal(nodeDef, attrs, pos)`. We duck-type it
 * so this module doesn't have to import the main class (and avoids a
 * circular import).
 *
 * @typedef {{ _openNodeEditModal: (def: CustomNodeDef, attrs: any, pos: number) => void }} HostAPI
 */

export class CustomNodeView {
  /**
   * @param {any} node ProseMirror node.
   * @param {any} view EditorView.
   * @param {() => number} getPos
   * @param {CustomNodeDef} nodeDef
   */
  constructor(node, view, getPos, nodeDef) {
    this.node = node;
    this.view = view;
    this.getPos = getPos;
    this.nodeDef = nodeDef;

    this.dom = h(nodeDef.inline ? 'span' : 'div', {
      class: ['crap-custom-node', nodeDef.inline && 'crap-custom-node--inline'],
      contentEditable: 'false',
    });
    this._render();

    this.dom.addEventListener('dblclick', (e) => {
      e.preventDefault();
      e.stopPropagation();
      this._findHost()?._openNodeEditModal(nodeDef, { ...this.node.attrs }, this.getPos());
    });
  }

  /**
   * Walk up the composed DOM tree to find the host element. Duck-typed
   * on `_openNodeEditModal` so this module doesn't need to import the
   * main class.
   *
   * @returns {HostAPI|null}
   */
  _findHost() {
    /** @type {Element|null} */
    let el = this.view.dom;
    while (el) {
      const root = el.getRootNode?.();
      if (root instanceof ShadowRoot) {
        const host = /** @type {any} */ (root.host);
        if (host && typeof host._openNodeEditModal === 'function') return host;
      }
      el = el.parentElement;
    }
    return null;
  }

  _render() {
    const label = this.nodeDef.label || this.nodeDef.name;
    const attrSummary = (this.nodeDef.attrs || [])
      .slice(0, 3)
      .map((a) => this.node.attrs[a.name])
      .filter((v) => v != null && v !== '')
      .join(' | ');

    const children = [h('span', { class: 'crap-custom-node__label', text: label })];
    if (attrSummary) {
      children.push(h('span', { class: 'crap-custom-node__attrs', text: attrSummary }));
    }
    this.dom.replaceChildren(...children);
  }

  /**
   * Called by ProseMirror when the node is updated.
   * @param {any} node
   */
  update(node) {
    if (node.type.name !== this.nodeDef.name) return false;
    this.node = node;
    this._render();
    return true;
  }

  selectNode() {
    this.dom.classList.add('ProseMirror-selectednode');
  }

  deselectNode() {
    this.dom.classList.remove('ProseMirror-selectednode');
  }

  stopEvent() {
    return true;
  }
}
