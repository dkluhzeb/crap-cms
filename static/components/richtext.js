/**
 * <crap-richtext> — ProseMirror-based WYSIWYG editor.
 *
 * Wraps a hidden `<textarea>` with a rich editor. The textarea remains
 * the form submission source — the editor syncs HTML (or PM-JSON when
 * `data-format="json"`) back on every change.
 *
 * Requires `window.ProseMirror` (loaded via the prosemirror.js IIFE
 * bundle). Falls back to showing the plain textarea if PM is missing.
 *
 * Internal organisation — the implementation is split across the
 * `static/components/richtext/` folder:
 *  - `styles.js`     — constructable stylesheet
 *  - `schema.js`     — `buildSchema` (nodes, marks, custom-nodes)
 *  - `plugins.js`    — `buildPlugins` (input rules, keymaps, history)
 *  - `toolbar.js`    — toolbar DOM + active-state helpers
 *  - `link-modal.js` — link insert/edit modal
 *  - `node-modal.js` — schema-driven custom-node edit modal
 *  - `node-view.js`  — `CustomNodeView` (PM NodeView class)
 *
 * @attr data-features  JSON array of enabled features. Absent ⇒ all on.
 *                      Available: `bold`, `italic`, `code`, `link`,
 *                      `heading`, `blockquote`, `orderedList`,
 *                      `bulletList`, `codeBlock`, `horizontalRule`.
 * @attr data-nodes     JSON array of custom node definitions.
 * @attr data-format    `"html"` (default) or `"json"` (PM doc JSON).
 * @attr data-no-resize Disable the editor's vertical resize handle.
 *
 * @example
 * <crap-richtext>
 *   <textarea name="content" hidden>...</textarea>
 * </crap-richtext>
 *
 * @module richtext
 * @stability stable
 */

import { h } from './_internal/h.js';
import { parseJsonAttribute } from './_internal/util/json.js';
import { openLinkModal } from './richtext/link-modal.js';
import { openNodeEditModal } from './richtext/node-modal.js';
import { CustomNodeView } from './richtext/node-view.js';
import { buildPlugins } from './richtext/plugins.js';
import { buildSchema } from './richtext/schema.js';
import { sheet } from './richtext/styles.js';
import {
  buildToolbarNodes,
  getMarkAttrs,
  isCommandActive,
  markActive,
} from './richtext/toolbar.js';

/**
 * @typedef {(name: string) => boolean} FeatureCheck
 *
 * @typedef {{
 *   name: string,
 *   label: string,
 *   inline?: boolean,
 *   attrs?: Array<{ name: string, type: string, label: string, [k: string]: any }>,
 * }} CustomNodeDef
 */

/**
 * Read the `data-features` attribute and return a Set of enabled
 * feature names, or `null` if all features should be enabled.
 *
 * @param {Element} host
 * @returns {Set<string>|null}
 */
function readEnabledFeatures(host) {
  const arr = parseJsonAttribute(host, 'data-features', null);
  return Array.isArray(arr) && arr.length > 0 ? new Set(arr) : null;
}

/**
 * Read the `data-nodes` attribute and return the parsed custom-node
 * definition list (or empty array if missing/malformed).
 *
 * @param {Element} host
 * @returns {CustomNodeDef[]}
 */
function readCustomNodes(host) {
  const arr = parseJsonAttribute(host, 'data-nodes', []);
  return Array.isArray(arr) ? arr : [];
}

class CrapRichtext extends HTMLElement {
  constructor() {
    super();
    /** @type {any} */
    this._view = null;
    /** @type {CustomNodeDef[]} */
    this._customNodes = [];
    /** @type {HTMLDivElement|null} */
    this._editorEl = null;
    this.attachShadow({ mode: 'open' });
  }

  /* ── Lifecycle ──────────────────────────────────────────────── */

  connectedCallback() {
    // Idempotency: skip re-init on DOM moves (e.g. array row drag-and-drop).
    if (this._view) return;

    const PM = /** @type {any} */ (window).ProseMirror;
    const textarea = /** @type {HTMLTextAreaElement|null} */ (this.querySelector('textarea'));
    if (!textarea) return;

    // Graceful degradation: no ProseMirror → leave the textarea visible.
    if (!PM) {
      textarea.hidden = false;
      return;
    }

    textarea.hidden = true;
    const has = this._buildFeatureCheck();
    this._customNodes = readCustomNodes(this);
    const schema = buildSchema(PM, has, this._customNodes);
    const format = this.getAttribute('data-format') || 'html';
    const doc = this._parseInitialDoc(PM, textarea, schema, format);
    const plugins = buildPlugins(PM, schema, has, (view) =>
      this._updateToolbar(view.state, schema, has),
    );
    const state = PM.EditorState.create({ doc, plugins });
    const isReadonly = textarea.hasAttribute('readonly');

    this._mountShadowTree(has, isReadonly);
    this._view = this._createEditorView(PM, state, schema, textarea, format, isReadonly);

    if (!isReadonly) this._bindToolbar(schema, has);
    this._updateToolbar(state, schema, has);
  }

  disconnectedCallback() {
    // Do NOT destroy the view — DOM moves trigger disconnect+reconnect, and we
    // want to preserve editor state (undo history, cursor, content). The
    // idempotency guard above prevents re-init on reconnect.
  }

  /* ── Init helpers ───────────────────────────────────────────── */

  /** @returns {FeatureCheck} */
  _buildFeatureCheck() {
    const enabled = readEnabledFeatures(this);
    return (name) => enabled === null || enabled.has(name);
  }

  /**
   * @param {any} PM
   * @param {HTMLTextAreaElement} textarea
   * @param {any} schema
   * @param {string} format
   */
  _parseInitialDoc(PM, textarea, schema, format) {
    if (format === 'json' && textarea.value.trim()) {
      try {
        return PM.Node.fromJSON(schema, JSON.parse(textarea.value));
      } catch {
        return schema.topNodeType.createAndFill();
      }
    }
    // SAFETY: innerHTML on a detached element is the standard ProseMirror
    // pattern for HTML deserialization. Detached elements don't fire event
    // handlers or execute scripts — no XSS risk from parsing stored content.
    const container = document.createElement('div');
    container.innerHTML = textarea.value || '';
    return PM.DOMParser.fromSchema(schema).parse(container);
  }

  /**
   * @param {FeatureCheck} has
   * @param {boolean} isReadonly
   */
  _mountShadowTree(has, isReadonly) {
    const root = /** @type {ShadowRoot} */ (this.shadowRoot);
    root.adoptedStyleSheets = [sheet];
    this._editorEl = h('div', { class: 'richtext__editor' });
    const toolbar = isReadonly
      ? null
      : h('div', { class: 'richtext__toolbar' }, ...buildToolbarNodes(has, this._customNodes));
    root.append(
      h(
        'div',
        {
          class: ['richtext', this.hasAttribute('data-no-resize') && 'richtext--no-resize'],
        },
        toolbar,
        this._editorEl,
      ),
    );
  }

  /**
   * @param {any} PM
   * @param {any} state
   * @param {any} schema
   * @param {HTMLTextAreaElement} textarea
   * @param {string} format
   * @param {boolean} isReadonly
   */
  _createEditorView(PM, state, schema, textarea, format, isReadonly) {
    return new PM.EditorView(this._editorEl, {
      state,
      editable: () => !isReadonly,
      nodeViews: Object.fromEntries(
        this._customNodes.map((nd) => [
          nd.name,
          (/** @type {any} */ node, /** @type {any} */ view, /** @type {any} */ getPos) =>
            new CustomNodeView(node, view, getPos, nd),
        ]),
      ),
      dispatchTransaction: (/** @type {any} */ tr) => {
        if (!this._view) return;
        const newState = this._view.state.apply(tr);
        this._view.updateState(newState);
        if (tr.docChanged) {
          textarea.value = this._serializeDoc(PM, schema, newState.doc, format);
        }
      },
    });
  }

  /**
   * @param {any} PM
   * @param {any} schema
   * @param {any} doc
   * @param {string} format
   */
  _serializeDoc(PM, schema, doc, format) {
    if (format === 'json') return JSON.stringify(doc.toJSON());
    const fragment = PM.DOMSerializer.fromSchema(schema).serializeFragment(doc.content);
    const div = document.createElement('div');
    div.appendChild(fragment);
    return div.innerHTML;
  }

  /* ── Public API: validation error UI ────────────────────────── */

  /**
   * Highlight custom nodes that have validation errors.
   * @param {Record<string, string[]>} errorMap Keyed by `"type#index"`.
   */
  markNodeErrors(errorMap) {
    this.clearNodeErrors();
    if (!this._view || !this.shadowRoot) return;

    const customNames = new Set(this._customNodes.map((nd) => nd.name));
    const nodeKeys = this._collectNodeKeys(customNames);
    const nodeEls = this.shadowRoot.querySelectorAll('.crap-custom-node');

    for (let i = 0; i < nodeKeys.length && i < nodeEls.length; i++) {
      const msgs = errorMap[nodeKeys[i]];
      if (!msgs?.length) continue;
      nodeEls[i].classList.add('crap-custom-node--error');
      nodeEls[i].setAttribute('title', msgs.join('\n'));
    }
  }

  clearNodeErrors() {
    if (!this.shadowRoot) return;
    for (const el of this.shadowRoot.querySelectorAll('.crap-custom-node--error')) {
      el.classList.remove('crap-custom-node--error');
      el.removeAttribute('title');
    }
  }

  /** @param {Set<string>} customNames */
  _collectNodeKeys(customNames) {
    /** @type {string[]} */
    const keys = [];
    /** @type {Record<string, number>} */
    const counts = {};
    this._view.state.doc.descendants((/** @type {any} */ node) => {
      if (customNames.has(node.type.name)) {
        const idx = counts[node.type.name] ?? 0;
        counts[node.type.name] = idx + 1;
        keys.push(`${node.type.name}#${idx}`);
      }
    });
    return keys;
  }

  /**
   * Document-order index of a node of `nodeType` at or near `pos`.
   *
   * @param {string} nodeType
   * @param {number} pos
   */
  _getNodeIndex(nodeType, pos) {
    /** @type {number[]} */
    const positions = [];
    this._view.state.doc.descendants((/** @type {any} */ node, /** @type {number} */ p) => {
      if (node.type.name === nodeType) positions.push(p);
    });
    const exact = positions.indexOf(pos);
    if (exact >= 0) return exact;
    let closestIdx = 0;
    let minDist = Infinity;
    for (let i = 0; i < positions.length; i++) {
      const dist = Math.abs(positions[i] - pos);
      if (dist < minDist) {
        minDist = dist;
        closestIdx = i;
      }
    }
    return closestIdx;
  }

  /* ── Toolbar ────────────────────────────────────────────────── */

  /**
   * @param {any} schema
   * @param {FeatureCheck} has
   */
  _bindToolbar(schema, has) {
    const PM = /** @type {any} */ (window).ProseMirror;
    const toolbar = this.shadowRoot?.querySelector('.richtext__toolbar');
    if (!toolbar) return;

    const commands = this._buildCommands(PM, schema, has);
    toolbar.addEventListener('click', (e) => {
      if (!(e.target instanceof Element)) return;
      const btn = /** @type {HTMLElement|null} */ (e.target.closest('button[data-cmd]'));
      const cmd = btn?.getAttribute('data-cmd');
      if (cmd && commands[cmd]) {
        commands[cmd]();
        this._view?.focus();
      }
    });
  }

  /**
   * @param {any} PM
   * @param {any} schema
   * @param {FeatureCheck} has
   * @returns {Record<string, () => void>}
   */
  _buildCommands(PM, schema, has) {
    /** @type {Record<string, () => void>} */
    const cmds = {};
    const stateFn = () => this._view.state;
    const dispatchFn = () => this._view.dispatch;

    if (has('bold') && schema.marks.strong) {
      cmds.bold = () => PM.toggleMark(schema.marks.strong)(stateFn(), dispatchFn());
    }
    if (has('italic') && schema.marks.em) {
      cmds.italic = () => PM.toggleMark(schema.marks.em)(stateFn(), dispatchFn());
    }
    if (has('code') && schema.marks.code) {
      cmds.code = () => PM.toggleMark(schema.marks.code)(stateFn(), dispatchFn());
    }
    if (has('link') && schema.marks.link) {
      cmds.link = () => {
        const state = stateFn();
        const markType = schema.marks.link;
        openLinkModal(
          this,
          schema,
          markActive(state, markType) ? getMarkAttrs(state, markType) : {},
        );
      };
    }
    if (has('heading') && schema.nodes.heading) {
      const setHeading = (/** @type {number} */ level) => () =>
        PM.setBlockType(schema.nodes.heading, { level })(stateFn(), dispatchFn());
      cmds.h1 = setHeading(1);
      cmds.h2 = setHeading(2);
      cmds.h3 = setHeading(3);
      cmds.paragraph = () => PM.setBlockType(schema.nodes.paragraph)(stateFn(), dispatchFn());
    }
    if (has('bulletList') && schema.nodes.bullet_list) {
      cmds.ul = () => PM.wrapInList(schema.nodes.bullet_list)(stateFn(), dispatchFn());
    }
    if (has('orderedList') && schema.nodes.ordered_list) {
      cmds.ol = () => PM.wrapInList(schema.nodes.ordered_list)(stateFn(), dispatchFn());
    }
    if (has('blockquote') && schema.nodes.blockquote) {
      cmds.blockquote = () => PM.wrapIn(schema.nodes.blockquote)(stateFn(), dispatchFn());
    }
    if (has('horizontalRule') && schema.nodes.horizontal_rule) {
      cmds.hr = () => {
        const state = stateFn();
        dispatchFn()(state.tr.replaceSelectionWith(schema.nodes.horizontal_rule.create()));
      };
    }
    cmds.undo = () => PM.undo(stateFn(), dispatchFn());
    cmds.redo = () => PM.redo(stateFn(), dispatchFn());

    for (const nd of this._customNodes) {
      cmds[`insert-${nd.name}`] = () => this._insertCustomNode(schema, nd);
    }
    return cmds;
  }

  /**
   * Insert a fresh custom node and open its edit modal at the inserted
   * position.
   *
   * @param {any} schema
   * @param {CustomNodeDef} nd
   */
  _insertCustomNode(schema, nd) {
    const view = this._view;
    const nodeType = schema.nodes[nd.name];
    if (!nodeType) return;
    const defaults = Object.fromEntries((nd.attrs || []).map((a) => [a.name, a.default ?? '']));
    view.dispatch(view.state.tr.replaceSelectionWith(nodeType.create(defaults)));

    // For block atoms, replaceSelectionWith may split paragraphs, so mapping
    // the old selection gives an unreliable position. The cursor lands after
    // the inserted node; find the last node of the matching type at or
    // before the new selection.
    const newState = view.state;
    const anchor = newState.selection.from;
    let nodePos = -1;
    newState.doc.descendants((/** @type {any} */ n, /** @type {number} */ p) => {
      if (n.type.name === nd.name && p <= anchor) nodePos = p;
    });
    if (nodePos >= 0) this._openNodeEditModal(nd, defaults, nodePos);
  }

  /**
   * @param {any} state
   * @param {any} schema
   * @param {FeatureCheck} has
   */
  _updateToolbar(state, schema, has) {
    const toolbar = this.shadowRoot?.querySelector('.richtext__toolbar');
    if (!toolbar) return;
    for (const btn of /** @type {NodeListOf<HTMLButtonElement>} */ (
      toolbar.querySelectorAll('button[data-cmd]')
    )) {
      btn.classList.toggle(
        'active',
        isCommandActive(btn.getAttribute('data-cmd') || '', state, schema, has),
      );
    }
  }

  /* ── Modal entry points (delegated to submodules) ───────────── */

  /**
   * @param {CustomNodeDef} nodeDef
   * @param {Record<string, any>} attrs
   * @param {number} pos
   */
  _openNodeEditModal(nodeDef, attrs, pos) {
    openNodeEditModal(this, nodeDef, attrs, pos);
  }

  /**
   * Apply attrs to the PM node at `pos` via `setNodeMarkup`. If
   * `expectedType` is given and the node at `pos` doesn't match, search
   * nearby positions for the correct node (positions can drift after
   * surrounding edits).
   *
   * @param {number} pos
   * @param {Record<string, any>} newAttrs
   * @param {string} [expectedType]
   */
  _applyNodeAttrs(pos, newAttrs, expectedType) {
    if (!this._view) return;
    const { state, dispatch } = this._view;
    try {
      let node = state.doc.nodeAt(pos);
      let resolvedPos = pos;
      if (expectedType && (!node || node.type.name !== expectedType)) {
        for (const offset of [1, -1, 2, -2, 3, -3]) {
          const tryPos = pos + offset;
          if (tryPos < 0 || tryPos >= state.doc.content.size) continue;
          const candidate = state.doc.nodeAt(tryPos);
          if (candidate && candidate.type.name === expectedType) {
            resolvedPos = tryPos;
            node = candidate;
            break;
          }
        }
      }
      if (node) dispatch(state.tr.setNodeMarkup(resolvedPos, null, newAttrs));
    } catch {
      /* position drifted out of bounds */
    }
  }
}

customElements.define('crap-richtext', CrapRichtext);
