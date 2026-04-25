import { h } from './h.js';
import { t } from './i18n.js';

/**
 * <crap-richtext> — ProseMirror-based WYSIWYG editor.
 *
 * Wraps a hidden <textarea> with a rich editor. The textarea remains
 * the form submission source — the editor syncs HTML back on every change.
 *
 * Requires `window.ProseMirror` (loaded via prosemirror.js IIFE bundle).
 * Falls back to showing the plain textarea if ProseMirror is unavailable.
 *
 * Supports `data-features` attribute (JSON array) to limit which toolbar
 * buttons and editing features are available. When absent, all features
 * are enabled.
 *
 * Available features: "bold", "italic", "code", "link", "heading",
 * "blockquote", "orderedList", "bulletList", "codeBlock", "horizontalRule"
 *
 * Supports `data-nodes` attribute (JSON array of custom node definitions)
 * for embedding structured components (CTAs, embeds, etc.) in the editor.
 *
 * @example
 * <crap-richtext>
 *   <textarea name="content" hidden>...</textarea>
 * </crap-richtext>
 *
 * <crap-richtext data-features='["bold","italic","heading","link"]'>
 *   <textarea name="content" hidden>...</textarea>
 * </crap-richtext>
 */

/**
 * Build a single field element for the custom-node edit modal.
 *
 * The schema-driven field renderer used to interpolate every value through
 * `_esc()` into `innerHTML` strings — that's the bug class we're closing.
 * Here every value flows through `setAttribute` / `textContent`, so HTML
 * injection is unwriteable regardless of what `attrs[a.name]` contains.
 *
 * @param {{ name: string, label: string }} nodeDef
 * @param {Record<string, any>} attrs - current attribute values (user data)
 * @param {{ name: string, type: string, label: string, required?: boolean, readonly?: boolean,
 *           default?: any, placeholder?: string, description?: string, width?: string,
 *           min?: any, max?: any, step?: any, min_length?: any, max_length?: any,
 *           min_date?: string, max_date?: string, language?: string, rows?: number,
 *           picker_appearance?: string, options?: Array<{value: string, label: string}> }} a
 * @returns {HTMLDivElement}
 */
function buildNodeField(nodeDef, attrs, a) {
  const val = attrs[a.name] ?? a.default ?? '';
  const inputId = `crap-node-${nodeDef.name}-${a.name}`;
  const ro = !!a.readonly;
  const req = !!a.required;
  const ph = a.placeholder || undefined;

  /** @type {HTMLElement} */
  let input;
  switch (a.type) {
    case 'textarea':
      input = h('textarea', {
        class: 'crap-node-modal__input',
        id: inputId,
        dataset: { attr: a.name },
        rows: a.rows || 3,
        placeholder: ph,
        required: req,
        readonly: ro,
        disabled: ro,
        minlength: a.min_length,
        maxlength: a.max_length,
      }, String(val));
      break;
    case 'checkbox':
      input = h('label', { class: 'crap-node-modal__checkbox' },
        h('input', {
          type: 'checkbox',
          id: inputId,
          dataset: { attr: a.name },
          checked: !!val,
          readonly: ro,
          disabled: ro,
        }),
        ` ${a.label}`,
      );
      break;
    case 'select':
      input = h('select', {
        class: 'crap-node-modal__input',
        id: inputId,
        dataset: { attr: a.name },
        required: req,
        readonly: ro,
        disabled: ro,
      }, ...(a.options || []).map(o =>
        h('option', { value: o.value, selected: o.value === val, text: o.label }),
      ));
      break;
    case 'radio':
      input = h('div', { class: 'crap-node-modal__radio-group', dataset: { attr: a.name } },
        ...(a.options || []).map((o, i) =>
          h('label', { class: 'crap-node-modal__radio' },
            h('input', {
              type: 'radio',
              id: `${inputId}-${i}`,
              name: `node-attr-${a.name}`,
              value: o.value,
              checked: o.value === val,
              readonly: ro,
              disabled: ro,
            }),
            ` ${o.label}`,
          ),
        ),
      );
      break;
    case 'number':
      input = h('input', {
        type: 'number',
        class: 'crap-node-modal__input',
        id: inputId,
        dataset: { attr: a.name },
        value: val,
        placeholder: ph,
        required: req,
        readonly: ro,
        disabled: ro,
        min: a.min,
        max: a.max,
        step: a.step,
      });
      break;
    case 'email':
      input = h('input', {
        type: 'email',
        class: 'crap-node-modal__input',
        id: inputId,
        dataset: { attr: a.name },
        value: val,
        placeholder: ph,
        required: req,
        readonly: ro,
        disabled: ro,
        minlength: a.min_length,
        maxlength: a.max_length,
      });
      break;
    case 'date': {
      let dateInputType = 'date';
      if (a.picker_appearance === 'dayAndTime') dateInputType = 'datetime-local';
      else if (a.picker_appearance === 'timeOnly') dateInputType = 'time';
      else if (a.picker_appearance === 'monthOnly') dateInputType = 'month';
      input = h('input', {
        type: dateInputType,
        class: 'crap-node-modal__input',
        id: inputId,
        dataset: { attr: a.name },
        value: val,
        required: req,
        readonly: ro,
        disabled: ro,
        min: a.min_date,
        max: a.max_date,
      });
      break;
    }
    case 'code':
    case 'json':
      input = h('textarea', {
        class: ['crap-node-modal__input', 'crap-node-modal__input--mono'],
        id: inputId,
        dataset: { attr: a.name },
        rows: a.rows || 4,
        placeholder: ph,
        required: req,
        readonly: ro,
        disabled: ro,
        minlength: a.min_length,
        maxlength: a.max_length,
      }, String(val));
      break;
    default:
      input = h('input', {
        type: 'text',
        class: 'crap-node-modal__input',
        id: inputId,
        dataset: { attr: a.name },
        value: val,
        placeholder: ph,
        required: req,
        readonly: ro,
        disabled: ro,
        minlength: a.min_length,
        maxlength: a.max_length,
      });
  }

  const desc = a.description
    ? h('p', { class: 'crap-node-modal__help', text: a.description })
    : null;
  const wrapper = h('div', {
    class: 'crap-node-modal__field',
    dataset: a.width ? { fieldWidth: a.width } : undefined,
  });

  if (a.type === 'checkbox') {
    wrapper.append(input);
    if (desc) wrapper.append(desc);
  } else {
    const langSuffix = a.language ? ` (${a.language})` : '';
    wrapper.append(
      h('label', {
        class: 'crap-node-modal__label',
        for: inputId,
        text: `${a.label}${langSuffix}${a.required ? ' *' : ''}`,
      }),
      input,
    );
    if (desc) wrapper.append(desc);
  }
  return wrapper;
}

class CrapRichtext extends HTMLElement {
  constructor() {
    super();

    /** @type {import('prosemirror-view').EditorView | null} */
    this._view = null;

    this.attachShadow({ mode: 'open' });
  }

  connectedCallback() {
    // Idempotency guard: skip re-init on DOM moves (e.g. array row drag-and-drop)
    if (this._view) return;

    const PM = /** @type {any} */ (window).ProseMirror;
    /** @type {HTMLTextAreaElement | null} */
    const textarea = this.querySelector('textarea');

    // Graceful degradation: no ProseMirror -> show plain textarea
    if (!PM || !textarea) {
      if (textarea) textarea.style.display = '';
      return;
    }

    textarea.style.display = 'none';

    // Parse enabled features (empty = all enabled)
    /** @type {Set<string>|null} */
    let enabledFeatures = null;
    const featuresAttr = this.getAttribute('data-features');
    if (featuresAttr) {
      try {
        const arr = JSON.parse(featuresAttr);
        if (Array.isArray(arr) && arr.length > 0) {
          enabledFeatures = new Set(arr);
        }
      } catch { /* ignore, all features enabled */ }
    }

    /**
     * Check if a feature is enabled.
     * @param {string} name
     * @returns {boolean}
     */
    const has = (name) => enabledFeatures === null || enabledFeatures.has(name);

    // Build schema — conditionally include nodes and marks
    const baseNodes = PM.basicSchema.spec.nodes;
    let nodes = baseNodes;

    // Add list nodes only if list features are enabled
    if (has('orderedList') || has('bulletList')) {
      nodes = PM.addListNodes(nodes, 'paragraph block*', 'block');
    }

    // Remove nodes based on features
    if (!has('heading')) {
      nodes = nodes.remove('heading');
    }
    if (!has('codeBlock')) {
      nodes = nodes.remove('code_block');
    }
    if (!has('blockquote')) {
      nodes = nodes.remove('blockquote');
    }
    if (!has('horizontalRule')) {
      nodes = nodes.remove('horizontal_rule');
    }

    // Build marks — conditionally include
    let marksObj = {};
    const baseMarks = PM.basicSchema.spec.marks;
    if (has('bold') && baseMarks.get('strong')) {
      marksObj.strong = baseMarks.get('strong');
    }
    if (has('italic') && baseMarks.get('em')) {
      marksObj.em = baseMarks.get('em');
    }
    if (has('code') && baseMarks.get('code')) {
      marksObj.code = baseMarks.get('code');
    }
    if (has('link')) {
      marksObj.link = {
        attrs: {
          href: { default: '' },
          title: { default: null },
          target: { default: null },
          rel: { default: null },
        },
        inclusive: false,
        parseDOM: [{
          tag: 'a[href]',
          getAttrs(dom) {
            return {
              href: dom.getAttribute('href'),
              title: dom.getAttribute('title'),
              target: dom.getAttribute('target'),
              rel: dom.getAttribute('rel'),
            };
          },
        }],
        toDOM(node) {
          const { href, title, target, rel } = node.attrs;
          const attrs = { href };
          if (title) attrs.title = title;
          if (target) attrs.target = target;
          if (rel) attrs.rel = rel;
          return ['a', attrs, 0];
        },
      };
    }

    // Parse custom nodes from data-nodes attribute
    /** @type {Array<{name: string, label: string, inline: boolean, attrs: Array<{name: string, type: string, label: string, required: boolean, default?: any, options?: Array<{label: string, value: string}>}>}>} */
    const customNodes = [];
    const nodesAttr = this.getAttribute('data-nodes');
    if (nodesAttr) {
      try {
        const parsed = JSON.parse(nodesAttr);
        if (Array.isArray(parsed)) customNodes.push(...parsed);
      } catch { /* ignore */ }
    }

    // Inject custom NodeSpecs into schema
    for (const nodeDef of customNodes) {
      nodes = nodes.addToEnd(nodeDef.name, {
        group: nodeDef.inline ? 'inline' : 'block',
        inline: nodeDef.inline,
        atom: true,
        attrs: Object.fromEntries(
          (nodeDef.attrs || []).map(a => [a.name, { default: a.default ?? '' }])
        ),
        toDOM(node) {
          return ['crap-node', {
            'data-type': nodeDef.name,
            'data-attrs': JSON.stringify(node.attrs),
          }];
        },
        parseDOM: [{
          tag: `crap-node[data-type="${nodeDef.name}"]`,
          getAttrs(dom) {
            try { return JSON.parse(dom.getAttribute('data-attrs') || '{}'); }
            catch { return {}; }
          },
        }],
      });
    }

    const schema = new PM.Schema({
      nodes,
      marks: marksObj,
    });

    // Read storage format: "html" (default) or "json" (ProseMirror JSON)
    const format = this.getAttribute('data-format') || 'html';

    // Parse existing content into a ProseMirror document
    let doc;
    if (format === 'json' && textarea.value.trim()) {
      try {
        const parsed = JSON.parse(textarea.value);
        doc = PM.Node.fromJSON(schema, parsed);
      } catch {
        // Fallback to empty doc on parse error
        doc = schema.topNodeType.createAndFill();
      }
    } else {
      // Safety: innerHTML on a detached element is acceptable here — standard
      // ProseMirror pattern. Detached elements don't fire event handlers or
      // execute scripts, so no XSS risk from parsing stored HTML content.
      const container = document.createElement('div');
      container.innerHTML = textarea.value || '';
      doc = PM.DOMParser.fromSchema(schema).parse(container);
    }

    const isReadonly = textarea.hasAttribute('readonly');

    // Input rules: smart quotes, em dash, ellipsis, plus conditional block-level rules
    const rules = [
      ...PM.smartQuotes,
      PM.emDash,
      PM.ellipsis,
    ];

    if (has('blockquote') && schema.nodes.blockquote) {
      rules.push(PM.wrappingInputRule(/^\s*>\s$/, schema.nodes.blockquote));
    }
    if (has('orderedList') && schema.nodes.ordered_list) {
      rules.push(PM.wrappingInputRule(
        /^(\d+)\.\s$/,
        schema.nodes.ordered_list,
        (match) => ({ order: +match[1] }),
        (match, node) => node.childCount + node.attrs.order === +match[1]
      ));
    }
    if (has('bulletList') && schema.nodes.bullet_list) {
      rules.push(PM.wrappingInputRule(/^\s*([-*])\s$/, schema.nodes.bullet_list));
    }
    if (has('codeBlock') && schema.nodes.code_block) {
      rules.push(PM.textblockTypeInputRule(/^```$/, schema.nodes.code_block));
    }
    if (has('heading') && schema.nodes.heading) {
      rules.push(PM.textblockTypeInputRule(
        /^(#{1,3})\s$/,
        schema.nodes.heading,
        (match) => ({ level: match[1].length })
      ));
    }

    // Keymap for list operations
    const listKeymap = {};
    if (schema.nodes.list_item) {
      listKeymap['Enter'] = PM.splitListItem(schema.nodes.list_item);
      listKeymap['Tab'] = PM.sinkListItem(schema.nodes.list_item);
      listKeymap['Shift-Tab'] = PM.liftListItem(schema.nodes.list_item);
    }

    // Keyboard shortcuts — only for enabled marks
    const markKeymap = {};
    markKeymap['Mod-z'] = PM.undo;
    markKeymap['Mod-shift-z'] = PM.redo;
    markKeymap['Mod-y'] = PM.redo;
    if (has('bold') && schema.marks.strong) {
      markKeymap['Mod-b'] = PM.toggleMark(schema.marks.strong);
    }
    if (has('italic') && schema.marks.em) {
      markKeymap['Mod-i'] = PM.toggleMark(schema.marks.em);
    }
    if (has('code') && schema.marks.code) {
      markKeymap['Mod-`'] = PM.toggleMark(schema.marks.code);
    }

    // Plugin to track active marks/nodes for toolbar state
    const toolbarPluginKey = new PM.PluginKey('toolbar');
    const toolbarPlugin = new PM.Plugin({
      key: toolbarPluginKey,
      view: () => ({
        update: (/** @type {any} */ view) => {
          this._updateToolbar(view.state, schema, has);
        },
      }),
    });

    const plugins = [
      PM.inputRules({ rules }),
      PM.keymap(listKeymap),
      PM.keymap(markKeymap),
      PM.keymap(PM.baseKeymap),
      PM.dropCursor(),
      PM.gapCursor(),
      PM.history(),
      toolbarPlugin,
    ];

    const state = PM.EditorState.create({ doc, plugins });

    // Render Shadow DOM
    this.shadowRoot.adoptedStyleSheets = [sheet];
    const editorEl = h('div', { class: 'richtext__editor' });
    const toolbar = isReadonly
      ? null
      : h('div', { class: 'richtext__toolbar' }, ...CrapRichtext._toolbarNodes(has, customNodes));
    this.shadowRoot.append(
      h('div', {
        class: ['richtext', this.hasAttribute('data-no-resize') && 'richtext--no-resize'],
      }, toolbar, editorEl),
    );

    // Store custom node defs on instance for toolbar/modal use
    /** @type {typeof customNodes} */
    this._customNodes = customNodes;

    this._view = new PM.EditorView(editorEl, {
      state,
      editable: () => !isReadonly,
      nodeViews: Object.fromEntries(
        customNodes.map(nd => [nd.name, (node, view, getPos) =>
          new CustomNodeView(node, view, getPos, nd)
        ])
      ),
      dispatchTransaction: (/** @type {any} */ tr) => {
        const newState = this._view.state.apply(tr);
        this._view.updateState(newState);
        if (tr.docChanged) {
          if (format === 'json') {
            textarea.value = JSON.stringify(newState.doc.toJSON());
          } else {
            const fragment = PM.DOMSerializer
              .fromSchema(schema)
              .serializeFragment(newState.doc.content);
            const div = document.createElement('div');
            div.appendChild(fragment);
            textarea.value = div.innerHTML;
          }
        }
      },
    });

    // Wire up toolbar buttons
    if (!isReadonly) {
      this._bindToolbar(schema, has);
    }

    // Initial toolbar state
    this._updateToolbar(state, schema, has);
  }

  disconnectedCallback() {
    // Do NOT destroy the view here — DOM moves (drag-and-drop reordering)
    // trigger disconnect+reconnect, and we want to preserve editor state
    // (undo history, cursor position, content). The idempotency guard in
    // connectedCallback prevents re-initialization on reconnect.
  }

  /**
   * Highlight custom nodes that have validation errors.
   * @param {Record<string, string[]>} errorMap - keyed by "type#index", values are error messages
   */
  markNodeErrors(errorMap) {
    this.clearNodeErrors();
    if (!this._view) return;

    // Only match registered custom node types — not built-in atoms like
    // text, hard_break, horizontal_rule which would break the zip alignment.
    const customNames = new Set((this._customNodes || []).map(nd => nd.name));

    // Build ordered list of type#index keys from the PM doc
    /** @type {string[]} */
    const nodeKeys = [];
    /** @type {Record<string, number>} */
    const typeCounts = {};
    this._view.state.doc.descendants((node) => {
      if (customNames.has(node.type.name)) {
        const name = node.type.name;
        const idx = typeCounts[name] ?? 0;
        typeCounts[name] = idx + 1;
        nodeKeys.push(`${name}#${idx}`);
      }
    });

    // Query custom node DOM elements in document order
    const nodeEls = this.shadowRoot.querySelectorAll('.crap-custom-node');

    // Zip — both follow document order
    for (let i = 0; i < nodeKeys.length && i < nodeEls.length; i++) {
      const msgs = errorMap[nodeKeys[i]];
      if (msgs && msgs.length > 0) {
        nodeEls[i].classList.add('crap-custom-node--error');
        nodeEls[i].title = msgs.join('\n');
      }
    }
  }

  /**
   * Remove error highlighting from all custom nodes.
   */
  clearNodeErrors() {
    if (!this.shadowRoot) return;
    const errorNodes = this.shadowRoot.querySelectorAll('.crap-custom-node--error');
    for (const el of errorNodes) {
      el.classList.remove('crap-custom-node--error');
      el.removeAttribute('title');
    }
  }

  /**
   * Get the document-order index of a node of `nodeType` at or near `pos`.
   * If `pos` doesn't exactly match, falls back to the closest node.
   * @param {string} nodeType - node type name
   * @param {number} pos - expected position of the node
   * @returns {number}
   */
  _getNodeIndex(nodeType, pos) {
    /** @type {number[]} */
    const positions = [];
    this._view.state.doc.descendants((node, nodePos) => {
      if (node.type.name === nodeType) positions.push(nodePos);
    });
    const exact = positions.indexOf(pos);
    if (exact >= 0) return exact;
    // Fallback: find the closest node of the same type
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

  /**
   * Bind click handlers to all toolbar buttons.
   * @param {any} schema - ProseMirror schema
   * @param {(name: string) => boolean} has - feature check
   */
  _bindToolbar(schema, has) {
    const PM = /** @type {any} */ (window).ProseMirror;
    const toolbar = this.shadowRoot.querySelector('.richtext__toolbar');
    if (!toolbar) return;

    /** @type {Record<string, () => void>} */
    const commands = {};

    if (has('bold') && schema.marks.strong) {
      commands.bold = () => PM.toggleMark(schema.marks.strong)(this._view.state, this._view.dispatch);
    }
    if (has('italic') && schema.marks.em) {
      commands.italic = () => PM.toggleMark(schema.marks.em)(this._view.state, this._view.dispatch);
    }
    if (has('code') && schema.marks.code) {
      commands.code = () => PM.toggleMark(schema.marks.code)(this._view.state, this._view.dispatch);
    }
    if (has('link') && schema.marks.link) {
      commands.link = () => {
        const { state } = this._view;
        const markType = schema.marks.link;
        if (this._markActive(state, markType)) {
          this._openLinkModal(schema, this._getMarkAttrs(state, markType));
        } else {
          this._openLinkModal(schema, {});
        }
      };
    }
    if (has('heading') && schema.nodes.heading) {
      commands.h1 = () => PM.setBlockType(schema.nodes.heading, { level: 1 })(this._view.state, this._view.dispatch);
      commands.h2 = () => PM.setBlockType(schema.nodes.heading, { level: 2 })(this._view.state, this._view.dispatch);
      commands.h3 = () => PM.setBlockType(schema.nodes.heading, { level: 3 })(this._view.state, this._view.dispatch);
      commands.paragraph = () => PM.setBlockType(schema.nodes.paragraph)(this._view.state, this._view.dispatch);
    }
    if (has('bulletList') && schema.nodes.bullet_list) {
      commands.ul = () => PM.wrapInList(schema.nodes.bullet_list)(this._view.state, this._view.dispatch);
    }
    if (has('orderedList') && schema.nodes.ordered_list) {
      commands.ol = () => PM.wrapInList(schema.nodes.ordered_list)(this._view.state, this._view.dispatch);
    }
    if (has('blockquote') && schema.nodes.blockquote) {
      commands.blockquote = () => PM.wrapIn(schema.nodes.blockquote)(this._view.state, this._view.dispatch);
    }
    if (has('horizontalRule') && schema.nodes.horizontal_rule) {
      commands.hr = () => {
        const { state, dispatch } = this._view;
        dispatch(state.tr.replaceSelectionWith(schema.nodes.horizontal_rule.create()));
      };
    }
    commands.undo = () => PM.undo(this._view.state, this._view.dispatch);
    commands.redo = () => PM.redo(this._view.state, this._view.dispatch);

    // Custom node insert commands
    for (const nd of (this._customNodes || [])) {
      commands[`insert-${nd.name}`] = () => {
        const nodeType = schema.nodes[nd.name];
        if (!nodeType) return;
        const defaultAttrs = Object.fromEntries(
          (nd.attrs || []).map(a => [a.name, a.default ?? ''])
        );
        const { state, dispatch } = this._view;
        const node = nodeType.create(defaultAttrs);
        const tr = state.tr.replaceSelectionWith(node);
        dispatch(tr);
        // Find the exact position of the inserted node in the updated state.
        // For block atoms, replaceSelectionWith may split paragraphs, so
        // mapping the old selection gives an unreliable position.
        // The cursor is always placed after the inserted node, so find the
        // last node of the matching type at or before the new selection.
        const newState = this._view.state;
        const anchor = newState.selection.from;
        let nodePos = -1;
        newState.doc.descendants((n, p) => {
          if (n.type.name === nd.name && p <= anchor) {
            nodePos = p;
          }
        });
        if (nodePos >= 0) {
          this._openNodeEditModal(nd, defaultAttrs, nodePos);
        }
      };
    }

    toolbar.addEventListener('click', (e) => {
      const btn = /** @type {HTMLElement} */ (e.target).closest('button[data-cmd]');
      if (!btn) return;
      const cmd = btn.getAttribute('data-cmd');
      if (cmd && commands[cmd]) {
        commands[cmd]();
        this._view.focus();
      }
    });
  }

  /**
   * Check if a mark is active in the current selection.
   * @param {any} state
   * @param {any} markType
   * @returns {boolean}
   */
  _markActive(state, markType) {
    const { from, $from, to, empty } = state.selection;
    if (empty) return !!markType.isInSet(state.storedMarks || $from.marks());
    return state.doc.rangeHasMark(from, to, markType);
  }

  /**
   * Extract attrs from the active mark at cursor position.
   * @param {any} state - ProseMirror editor state
   * @param {any} markType - ProseMirror mark type
   * @returns {object} mark attrs or empty object
   */
  _getMarkAttrs(state, markType) {
    const marks = state.storedMarks || state.selection.$from.marks();
    const mark = markType.isInSet(marks);
    return mark ? { ...mark.attrs } : {};
  }

  /**
   * Open a modal for inserting or editing a link.
   * @param {any} schema - ProseMirror schema
   * @param {object} attrs - current link attrs (empty for insert mode)
   */
  _openLinkModal(schema, attrs) {
    const PM = /** @type {any} */ (window).ProseMirror;
    const existing = this.shadowRoot.querySelector('.crap-node-modal');
    if (existing) existing.remove();

    const isEdit = !!attrs.href;
    const savedSelection = this._view.state.selection;

    const modal = h('dialog', {
      class: 'crap-node-modal',
      'aria-labelledby': 'crap-link-modal-heading',
    });

    const fieldRow = (/** @type {Node[]} */ ...children) =>
      h('div', { class: 'crap-node-modal__field' }, ...children);

    /** @type {HTMLElement[]} */
    const footerButtons = [];
    if (isEdit) {
      footerButtons.push(h('button', {
        type: 'button',
        class: ['crap-node-modal__btn', 'crap-node-modal__btn--danger'],
        text: t('remove_link'),
      }));
    }
    footerButtons.push(h('button', {
      type: 'button',
      class: ['crap-node-modal__btn', 'crap-node-modal__btn--cancel'],
      text: t('cancel'),
    }));
    footerButtons.push(h('button', {
      type: 'button',
      class: ['crap-node-modal__btn', 'crap-node-modal__btn--ok'],
      text: t('apply'),
    }));

    modal.append(
      h('div', { class: 'crap-node-modal__dialog' },
        h('div', {
          class: 'crap-node-modal__header',
          id: 'crap-link-modal-heading',
          text: isEdit ? t('edit_link') : t('insert_link'),
        }),
        h('div', { class: 'crap-node-modal__body' },
          fieldRow(
            h('label', {
              class: 'crap-node-modal__label',
              for: 'crap-link-href',
              text: `${t('link_url')} *`,
            }),
            h('input', {
              type: 'url',
              class: 'crap-node-modal__input',
              id: 'crap-link-href',
              dataset: { field: 'href' },
              value: attrs.href || '',
              required: true,
            }),
          ),
          fieldRow(
            h('label', {
              class: 'crap-node-modal__label',
              for: 'crap-link-title',
              text: t('link_title'),
            }),
            h('input', {
              type: 'text',
              class: 'crap-node-modal__input',
              id: 'crap-link-title',
              dataset: { field: 'title' },
              value: attrs.title || '',
            }),
          ),
          fieldRow(
            h('label', { class: 'crap-node-modal__checkbox' },
              h('input', {
                type: 'checkbox',
                dataset: { field: 'target' },
                checked: attrs.target === '_blank',
              }),
              ` ${t('link_open_new_tab')}`,
            ),
          ),
          fieldRow(
            h('label', { class: 'crap-node-modal__checkbox' },
              h('input', {
                type: 'checkbox',
                dataset: { field: 'rel' },
                checked: !!(attrs.rel && attrs.rel.includes('nofollow')),
              }),
              ` ${t('link_nofollow')}`,
            ),
          ),
        ),
        h('div', {
          class: ['crap-node-modal__footer', isEdit && 'crap-node-modal__footer--with-remove'],
        }, ...footerButtons),
      ),
    );

    this.shadowRoot.appendChild(modal);
    modal.showModal();

    const hrefInput = modal.querySelector('[data-field="href"]');
    if (hrefInput) hrefInput.focus();

    const close = () => { modal.close(); modal.remove(); };

    const applyLink = () => {
      const hrefEl = modal.querySelector('[data-field="href"]');
      const href = hrefEl ? hrefEl.value.trim() : '';
      if (!href) return;

      // Block dangerous protocols (javascript:, data:, vbscript:)
      const proto = href.split(':')[0].toLowerCase().trim();
      const allowed = ['http', 'https', 'mailto', 'tel', ''];
      if (href.includes(':') && !allowed.includes(proto)) return;

      const titleEl = modal.querySelector('[data-field="title"]');
      const title = titleEl ? titleEl.value.trim() || null : null;
      const targetEl = modal.querySelector('[data-field="target"]');
      const target = targetEl && targetEl.checked ? '_blank' : null;
      const relEl = modal.querySelector('[data-field="rel"]');
      // Preserve existing rel tokens (e.g. noopener, noreferrer) while toggling nofollow
      const existingRel = (attrs.rel || '').split(/\s+/).filter(Boolean);
      const otherTokens = existingRel.filter((t) => t !== 'nofollow');
      const relTokens = relEl && relEl.checked ? ['nofollow', ...otherTokens] : otherTokens;
      const rel = relTokens.length > 0 ? relTokens.join(' ') : null;

      const markType = schema.marks.link;
      let { tr } = this._view.state;
      tr = tr.setSelection(savedSelection);

      const { from, to } = savedSelection;
      if (isEdit) {
        tr = tr.removeMark(from, to, markType);
      }
      tr = tr.addMark(from, to, markType.create({ href, title, target, rel }));

      this._view.dispatch(tr);
      close();
      this._view.focus();
    };

    const removeLink = () => {
      const markType = schema.marks.link;
      let { tr } = this._view.state;
      tr = tr.setSelection(savedSelection);
      const { from, to } = savedSelection;
      tr = tr.removeMark(from, to, markType);
      this._view.dispatch(tr);
      close();
      this._view.focus();
    };

    modal.addEventListener('cancel', (e) => { e.preventDefault(); close(); });
    modal.querySelector('.crap-node-modal__btn--cancel').addEventListener('click', close);
    modal.querySelector('.crap-node-modal__btn--ok').addEventListener('click', applyLink);

    const dangerBtn = modal.querySelector('.crap-node-modal__btn--danger');
    if (dangerBtn) dangerBtn.addEventListener('click', removeLink);

    hrefInput.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') { e.preventDefault(); applyLink(); }
    });
  }

  /**
   * Update toolbar button active states based on current editor state.
   * @param {any} state
   * @param {any} schema
   * @param {(name: string) => boolean} has - feature check
   */
  _updateToolbar(state, schema, has) {
    const toolbar = this.shadowRoot?.querySelector('.richtext__toolbar');
    if (!toolbar) return;

    /** @type {NodeListOf<HTMLButtonElement>} */
    const buttons = toolbar.querySelectorAll('button[data-cmd]');

    buttons.forEach((btn) => {
      const cmd = btn.getAttribute('data-cmd');
      let active = false;

      switch (cmd) {
        case 'bold':
          active = has('bold') && schema.marks.strong && this._markActive(state, schema.marks.strong);
          break;
        case 'italic':
          active = has('italic') && schema.marks.em && this._markActive(state, schema.marks.em);
          break;
        case 'code':
          active = has('code') && schema.marks.code && this._markActive(state, schema.marks.code);
          break;
        case 'link':
          active = has('link') && schema.marks.link && this._markActive(state, schema.marks.link);
          break;
        case 'h1':
        case 'h2':
        case 'h3': {
          if (has('heading') && schema.nodes.heading) {
            const level = parseInt(cmd[1]);
            const { $from } = state.selection;
            active = $from.parent.type === schema.nodes.heading && $from.parent.attrs.level === level;
          }
          break;
        }
        case 'paragraph': {
          const { $from } = state.selection;
          active = $from.parent.type === schema.nodes.paragraph;
          break;
        }
      }

      btn.classList.toggle('active', active);
    });
  }

  /**
   * Open the edit modal for a custom node at the given position.
   * @param {object} nodeDef - custom node definition
   * @param {object} attrs - current attribute values
   * @param {number} pos - node position in the document
   */
  _openNodeEditModal(nodeDef, attrs, pos) {
    // Remove any existing modal
    const existing = this.shadowRoot.querySelector('.crap-node-modal');
    if (existing) existing.remove();

    const modal = h('dialog', {
      class: 'crap-node-modal',
      'aria-labelledby': 'crap-node-modal-heading',
    });

    const formFields = (nodeDef.attrs || [])
      .filter(a => !a.hidden)
      .map(a => buildNodeField(nodeDef, attrs, a));

    modal.append(
      h('div', { class: 'crap-node-modal__dialog' },
        h('div', {
          class: 'crap-node-modal__header',
          id: 'crap-node-modal-heading',
          text: nodeDef.label,
        }),
        h('div', { class: 'crap-node-modal__body' }, ...formFields),
        h('div', { class: 'crap-node-modal__footer' },
          h('button', {
            type: 'button',
            class: ['crap-node-modal__btn', 'crap-node-modal__btn--cancel'],
            text: t('cancel'),
          }),
          h('button', {
            type: 'button',
            class: ['crap-node-modal__btn', 'crap-node-modal__btn--ok'],
            text: t('ok'),
          }),
        ),
      ),
    );

    this.shadowRoot.appendChild(modal);

    // Apply per-field widths programmatically (style attribute would need
    // escaping; data-attr + JS sidesteps that and is CSP-style-src-clean).
    for (const fieldEl of modal.querySelectorAll('[data-field-width]')) {
      fieldEl.style.width = fieldEl.dataset.fieldWidth;
    }

    modal.showModal();

    // Focus first input
    const firstInput = modal.querySelector('input, textarea, select');
    if (firstInput) firstInput.focus();

    const close = () => { modal.close(); modal.remove(); };

    modal.addEventListener('cancel', (e) => { e.preventDefault(); close(); });
    modal.querySelector('.crap-node-modal__btn--cancel').addEventListener('click', close);
    modal.querySelector('.crap-node-modal__btn--ok').addEventListener('click', async () => {
      // Collect new attrs from dialog fields
      const newAttrs = {};
      for (const a of (nodeDef.attrs || [])) {
        if (a.hidden) {
          newAttrs[a.name] = attrs[a.name] ?? a.default ?? '';
          continue;
        }
        const el = modal.querySelector(`[data-attr="${a.name}"]`);
        if (!el) continue;
        if (a.type === 'checkbox') {
          newAttrs[a.name] = el.checked;
        } else if (a.type === 'radio') {
          const checked = el.querySelector('input[type="radio"]:checked');
          newAttrs[a.name] = checked ? checked.value : '';
        } else {
          newAttrs[a.name] = el.value;
        }
      }

      // Find the validation form
      const validateForm = this.closest('crap-validate-form');
      if (!validateForm || typeof validateForm.getValidationErrors !== 'function') {
        // No validation available — apply and close
        this._applyNodeAttrs(pos, newAttrs, nodeDef.name);
        close();
        this._view.focus();
        return;
      }

      // Apply new attrs so the textarea serializes correctly for validation
      this._applyNodeAttrs(pos, newAttrs, nodeDef.name);

      // Disable OK button and show loading state
      const okBtn = modal.querySelector('.crap-node-modal__btn--ok');
      okBtn.disabled = true;
      okBtn.textContent = t('validating');
      CrapRichtext._clearDialogErrors(modal);

      const errors = await validateForm.getValidationErrors();

      if (errors === null) {
        // Network error — keep new attrs, close gracefully
        close();
        this._view.focus();
        return;
      }

      // Determine this node's error prefix: fieldName[nodeType#index]
      const textarea = this.querySelector('textarea');
      const fieldName = textarea ? textarea.name : '';
      const nodeIndex = this._getNodeIndex(nodeDef.name, pos);
      const prefix = `${fieldName}[${nodeDef.name}#${nodeIndex}].`;

      // Filter errors matching this node
      /** @type {Record<string, string>} */
      const attrErrors = {};
      for (const [key, message] of Object.entries(errors)) {
        if (key.startsWith(prefix)) {
          attrErrors[key.slice(prefix.length)] = message;
        }
      }

      if (Object.keys(attrErrors).length === 0) {
        // No errors for this node — close
        close();
        this._view.focus();
        return;
      }

      // Validation failed — revert to original attrs
      this._applyNodeAttrs(pos, attrs, nodeDef.name);

      // Show errors on dialog fields
      CrapRichtext._showDialogErrors(modal, attrErrors);

      // Re-enable OK button
      okBtn.disabled = false;
      okBtn.textContent = t('ok');
    });
  }

  /**
   * Apply attrs to the PM node at pos via setNodeMarkup.
   * If `expectedType` is given and the node at `pos` doesn't match,
   * searches nearby positions for the correct node.
   * @param {number} pos
   * @param {object} newAttrs
   * @param {string} [expectedType]
   */
  _applyNodeAttrs(pos, newAttrs, expectedType) {
    const { state, dispatch } = this._view;
    try {
      let node = state.doc.nodeAt(pos);
      if (expectedType && (!node || node.type.name !== expectedType)) {
        for (const offset of [1, -1, 2, -2, 3, -3]) {
          const tryPos = pos + offset;
          if (tryPos >= 0 && tryPos < state.doc.content.size) {
            const candidate = state.doc.nodeAt(tryPos);
            if (candidate && candidate.type.name === expectedType) {
              pos = tryPos;
              node = candidate;
              break;
            }
          }
        }
      }
      if (node) {
        dispatch(state.tr.setNodeMarkup(pos, null, newAttrs));
      }
    } catch { /* position might have changed */ }
  }

  /**
   * Show per-field errors in the node edit dialog.
   * @param {HTMLElement} modal
   * @param {Record<string, string>} attrErrors - keyed by attr name
   */
  static _showDialogErrors(modal, attrErrors) {
    CrapRichtext._clearDialogErrors(modal);
    for (const [attrName, message] of Object.entries(attrErrors)) {
      const input = modal.querySelector(`[data-attr="${attrName}"]`);
      if (!input) continue;
      input.classList.add('crap-node-modal__input--error');
      const errorEl = document.createElement('p');
      errorEl.className = 'crap-node-modal__error';
      errorEl.textContent = message;
      // Insert after the input, inside the .crap-node-modal__field wrapper
      const field = input.closest('.crap-node-modal__field');
      if (field) {
        field.appendChild(errorEl);
      }
    }
  }

  /**
   * Clear all error indicators from the node edit dialog.
   * @param {HTMLElement} modal
   */
  static _clearDialogErrors(modal) {
    for (const el of modal.querySelectorAll('.crap-node-modal__error')) {
      el.remove();
    }
    for (const el of modal.querySelectorAll('.crap-node-modal__input--error')) {
      el.classList.remove('crap-node-modal__input--error');
    }
  }

  /**
   * Generate toolbar group nodes based on enabled features.
   * @param {(name: string) => boolean} has - feature check
   * @param {Array<{name: string, label: string}>} [customNodes] - custom node defs
   * @returns {HTMLElement[]}
   */
  static _toolbarNodes(has, customNodes) {
    /** @type {HTMLElement[]} */
    const groups = [];

    /**
     * @param {string} cmd
     * @param {string} title
     * @param {Node | string} content
     */
    const btn = (cmd, title, content) =>
      h('button', { type: 'button', dataset: { cmd }, title }, content);

    const group = (/** @type {HTMLElement[]} */ buttons) =>
      h('div', { class: 'richtext__toolbar-group' }, ...buttons);

    // Inline marks group
    /** @type {HTMLElement[]} */
    const inlineButtons = [];
    if (has('bold')) inlineButtons.push(btn('bold', 'Bold (Ctrl+B)', h('strong', { text: 'B' })));
    if (has('italic')) inlineButtons.push(btn('italic', 'Italic (Ctrl+I)', h('em', { text: 'I' })));
    if (has('code')) inlineButtons.push(btn('code', 'Inline code (Ctrl+`)', h('code', { text: '</>' })));
    if (has('link')) inlineButtons.push(btn('link', 'Link', 'Link'));
    if (inlineButtons.length > 0) groups.push(group(inlineButtons));

    // Block type group
    /** @type {HTMLElement[]} */
    const blockButtons = [];
    if (has('heading')) {
      blockButtons.push(btn('h1', 'Heading 1', 'H1'));
      blockButtons.push(btn('h2', 'Heading 2', 'H2'));
      blockButtons.push(btn('h3', 'Heading 3', 'H3'));
      blockButtons.push(btn('paragraph', 'Paragraph', 'P'));
    }
    if (blockButtons.length > 0) groups.push(group(blockButtons));

    // List/block group
    /** @type {HTMLElement[]} */
    const listButtons = [];
    if (has('bulletList')) listButtons.push(btn('ul', 'Bullet list', 'UL'));
    if (has('orderedList')) listButtons.push(btn('ol', 'Ordered list', 'OL'));
    if (has('blockquote')) listButtons.push(btn('blockquote', 'Blockquote', 'Quote'));
    if (has('horizontalRule')) listButtons.push(btn('hr', 'Horizontal rule', 'HR'));
    if (listButtons.length > 0) groups.push(group(listButtons));

    // Custom node insert buttons
    if (customNodes && customNodes.length > 0) {
      groups.push(group(
        customNodes.map(nd => btn(`insert-${nd.name}`, `Insert ${nd.label}`, nd.label)),
      ));
    }

    // Undo/redo always present
    groups.push(group([
      btn('undo', 'Undo (Ctrl+Z)', 'Undo'),
      btn('redo', 'Redo (Ctrl+Shift+Z)', 'Redo'),
    ]));

    return groups;
  }

  /**
   * Shadow DOM styles. Uses CSS custom properties from :root (penetrate shadow boundary).
   * @returns {string}
   */
  static _styles() {
    return `
      :host {
        display: block;
      }

      .richtext {
        border: 1px solid var(--input-border, #e0e0e0);
        border-radius: var(--radius-md, 6px);
        background: var(--input-bg, #fff);
        box-shadow: var(--shadow-sm, 0 1px 2px rgba(0,0,0,0.04));
        overflow: hidden;
        display: flex;
        flex-direction: column;
        resize: vertical;
      }

      .richtext--no-resize {
        resize: none;
        max-height: 37.5rem;
      }

      .richtext:focus-within {
        border-color: var(--color-primary, #1677ff);
        box-shadow: 0 0 0 2px var(--color-primary-bg, rgba(22, 119, 255, 0.06));
      }

      /* -- Toolbar -- */

      .richtext__toolbar {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2xs, 2px);
        padding: 0.375rem var(--space-sm, 8px);
        border-bottom: 1px solid var(--border-color, #e0e0e0);
      }

      .richtext__toolbar-group {
        display: flex;
        gap: var(--space-2xs, 2px);
      }

      .richtext__toolbar-group:not(:last-child)::after {
        content: '';
        width: 1px;
        margin: var(--space-2xs, 2px) var(--space-xs, 4px);
        background: var(--border-color, #e0e0e0);
      }

      .richtext__toolbar button {
        all: unset;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        min-width: 1.75rem;
        height: 1.75rem;
        padding: 0 0.375rem;
        border-radius: var(--radius-sm, 4px);
        font-family: inherit;
        font-size: var(--text-xs, 0.75rem);
        font-weight: 500;
        color: var(--text-secondary, rgba(0, 0, 0, 0.65));
        cursor: pointer;
        box-sizing: border-box;
      }

      .richtext__toolbar button:hover {
        background: var(--bg-hover, rgba(0, 0, 0, 0.04));
        color: var(--text-primary, rgba(0, 0, 0, 0.88));
      }

      .richtext__toolbar button.active {
        background: var(--color-primary-bg, rgba(22, 119, 255, 0.06));
        color: var(--color-primary, #1677ff);
      }

      .richtext__toolbar button code {
        font-family: monospace;
        font-size: var(--text-xs, 0.75rem);
      }

      /* -- Editor area -- */

      .richtext__editor {
        min-height: 12.5rem;
        overflow-y: auto;
        flex: 1;
      }

      .richtext__editor .ProseMirror {
        padding: var(--space-md, 0.75rem) var(--space-lg, 1rem);
        min-height: 12.5rem;
        outline: none;
        font-family: inherit;
        font-size: var(--text-base, 0.875rem);
        line-height: 1.6;
        color: var(--text-primary, rgba(0, 0, 0, 0.88));
      }

      /* ProseMirror content styles */

      .richtext__editor .ProseMirror p {
        margin: 0 0 0.75em;
      }

      .richtext__editor .ProseMirror p:last-child {
        margin-bottom: 0;
      }

      .richtext__editor .ProseMirror h1,
      .richtext__editor .ProseMirror h2,
      .richtext__editor .ProseMirror h3 {
        margin: 1em 0 0.5em;
        font-weight: 600;
        line-height: 1.3;
      }

      .richtext__editor .ProseMirror h1:first-child,
      .richtext__editor .ProseMirror h2:first-child,
      .richtext__editor .ProseMirror h3:first-child {
        margin-top: 0;
      }

      .richtext__editor .ProseMirror h1 { font-size: 1.5em; }
      .richtext__editor .ProseMirror h2 { font-size: 1.25em; }
      .richtext__editor .ProseMirror h3 { font-size: 1.1em; }

      .richtext__editor .ProseMirror strong { font-weight: 600; }

      .richtext__editor .ProseMirror code {
        background: var(--bg-hover, rgba(0, 0, 0, 0.06));
        padding: 0.15em 0.35em;
        border-radius: 3px;
        font-family: monospace;
        font-size: 0.9em;
      }

      .richtext__editor .ProseMirror pre {
        background: var(--bg-hover, rgba(0, 0, 0, 0.04));
        border-radius: var(--radius-sm, 4px);
        padding: var(--space-md, 0.75rem) var(--space-lg, 1rem);
        margin: 0.75em 0;
        overflow-x: auto;
      }

      .richtext__editor .ProseMirror pre code {
        background: none;
        padding: 0;
        border-radius: 0;
      }

      .richtext__editor .ProseMirror blockquote {
        border-left: 3px solid var(--border-color-hover, #d9d9d9);
        margin: 0.75em 0;
        padding-left: 1em;
        color: var(--text-secondary, rgba(0, 0, 0, 0.65));
      }

      .richtext__editor .ProseMirror ul,
      .richtext__editor .ProseMirror ol {
        margin: 0.75em 0;
        padding-left: 1.5em;
      }

      .richtext__editor .ProseMirror li {
        margin-bottom: 0.25em;
      }

      .richtext__editor .ProseMirror li p {
        margin: 0;
      }

      .richtext__editor .ProseMirror hr {
        border: none;
        border-top: 1px solid var(--border-color, #e0e0e0);
        margin: 1em 0;
      }

      .richtext__editor .ProseMirror a {
        color: var(--color-primary, #1677ff);
        text-decoration: underline;
      }

      .richtext__editor .ProseMirror img {
        max-width: 100%;
      }

      /* ProseMirror plugin styles */

      .ProseMirror-gapcursor {
        display: none;
        pointer-events: none;
        position: absolute;
      }

      .ProseMirror-gapcursor:after {
        content: '';
        display: block;
        position: absolute;
        top: -2px;
        width: 1.25rem;
        border-top: 1px solid var(--text-primary, black);
        animation: ProseMirror-cursor-blink 1.1s steps(2, start) infinite;
      }

      @keyframes ProseMirror-cursor-blink {
        to { visibility: hidden; }
      }

      .ProseMirror-focused .ProseMirror-gapcursor {
        display: block;
      }

      .ProseMirror .ProseMirror-selectednode {
        outline: 2px solid var(--color-primary, #1677ff);
      }

      /* -- Custom node cards/pills -- */

      .crap-custom-node {
        display: flex;
        align-items: center;
        gap: var(--space-sm, 0.5rem);
        padding: var(--space-sm, 0.5rem) var(--space-md, 0.75rem);
        margin: var(--space-xs, 0.25rem) 0;
        border: 1px solid var(--border-color, #e0e0e0);
        border-radius: var(--radius-sm, 4px);
        background: var(--bg-hover, rgba(0, 0, 0, 0.02));
        cursor: pointer;
        user-select: none;
      }

      .crap-custom-node--inline {
        display: inline-flex;
        margin: 0 var(--space-2xs, 2px);
        padding: var(--space-2xs, 2px) var(--space-sm, 0.5rem);
        vertical-align: middle;
        border-radius: var(--radius-xl, 12px);
        font-size: 0.9em;
      }

      .crap-custom-node__label {
        font-weight: 600;
        font-size: 0.75em;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        color: var(--color-primary, #1677ff);
        white-space: nowrap;
      }

      .crap-custom-node__attrs {
        font-size: 0.85em;
        color: var(--text-secondary, rgba(0, 0, 0, 0.65));
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      /* -- Node edit modal -- */

      .crap-node-modal {
        border: none;
        padding: 0;
        width: 25rem;
        max-width: 90vw;
        max-height: 80vh;
        overflow-y: auto;
        background: var(--surface-primary, #fff);
        border-radius: var(--radius-md, 6px);
        box-shadow: var(--shadow-lg, 0 8px 24px rgba(0,0,0,0.12));
      }

      .crap-node-modal::backdrop {
        background: rgba(0, 0, 0, 0.3);
      }

      .crap-node-modal__dialog {
      }

      .crap-node-modal__header {
        padding: var(--space-lg, 1rem) 1.25rem;
        font-weight: 600;
        font-size: var(--text-base, 0.875rem);
        border-bottom: 1px solid var(--border-color, #e0e0e0);
      }

      .crap-node-modal__body {
        padding: var(--space-lg, 1rem) 1.25rem;
        display: flex;
        flex-direction: column;
        gap: var(--space-md, 0.75rem);
      }

      .crap-node-modal__field {
        display: flex;
        flex-direction: column;
        gap: var(--space-xs, 0.25rem);
      }

      .crap-node-modal__label {
        font-size: var(--text-xs, 0.75rem);
        font-weight: 500;
        color: var(--text-secondary, rgba(0, 0, 0, 0.65));
      }

      .crap-node-modal__input,
      .crap-node-modal__field select,
      .crap-node-modal__field textarea {
        padding: 0.375rem 0.625rem;
        border: 1px solid var(--input-border, #e0e0e0);
        border-radius: var(--radius-sm, 4px);
        font-family: inherit;
        font-size: var(--text-sm, 0.8125rem);
        background: var(--input-bg, #fff);
        color: var(--text-primary, rgba(0, 0, 0, 0.88));
      }

      .crap-node-modal__input:focus,
      .crap-node-modal__field select:focus,
      .crap-node-modal__field textarea:focus {
        outline: none;
        border-color: var(--color-primary, #1677ff);
        box-shadow: 0 0 0 2px var(--color-primary-bg, rgba(22, 119, 255, 0.06));
      }

      .crap-node-modal__checkbox {
        display: flex;
        align-items: center;
        gap: var(--space-sm, 0.5rem);
        font-size: var(--text-sm, 0.8125rem);
        cursor: pointer;
      }

      .crap-node-modal__radio-group {
        display: flex;
        flex-direction: column;
        gap: var(--space-xs, 0.25rem);
      }

      .crap-node-modal__radio {
        display: flex;
        align-items: center;
        gap: var(--space-sm, 0.5rem);
        font-size: var(--text-sm, 0.8125rem);
        cursor: pointer;
      }

      .crap-node-modal__input--mono {
        font-family: monospace;
        font-size: var(--text-xs, 0.75rem);
      }

      .crap-node-modal__help {
        margin: 0;
        font-size: var(--text-xs, 0.75rem);
        color: var(--text-tertiary, rgba(0, 0, 0, 0.45));
      }

      .crap-node-modal__footer {
        display: flex;
        justify-content: flex-end;
        gap: var(--space-sm, 0.5rem);
        padding: var(--space-md, 0.75rem) 1.25rem;
        border-top: 1px solid var(--border-color, #e0e0e0);
      }

      .crap-node-modal__btn {
        all: unset;
        display: inline-flex;
        align-items: center;
        height: var(--button-height-sm, 1.75rem);
        padding: 0 var(--space-lg, 1rem);
        border-radius: var(--radius-sm, 4px);
        font-family: inherit;
        font-size: var(--text-sm, 0.8125rem);
        font-weight: 500;
        cursor: pointer;
      }

      .crap-node-modal__btn--cancel {
        color: var(--text-secondary, rgba(0, 0, 0, 0.65));
      }

      .crap-node-modal__btn--cancel:hover {
        background: var(--bg-hover, rgba(0, 0, 0, 0.04));
      }

      .crap-node-modal__btn--ok {
        background: var(--color-primary, #1677ff);
        color: var(--text-on-primary, #fff);
      }

      .crap-node-modal__btn--ok:hover {
        opacity: 0.9;
      }

      .crap-node-modal__footer--with-remove {
        justify-content: flex-start;
      }

      .crap-node-modal__footer--with-remove .crap-node-modal__btn--cancel {
        margin-left: auto;
      }

      .crap-node-modal__btn--danger {
        color: var(--color-danger, #dc3545);
      }

      .crap-node-modal__btn--danger:hover {
        background: rgba(220, 53, 69, 0.08);
      }

      /* -- Node error states -- */

      .crap-custom-node--error {
        border-color: var(--color-danger, #dc3545);
        background: var(--color-danger-bg, rgba(220, 53, 69, 0.04));
      }

      .crap-custom-node--error .crap-custom-node__label {
        color: var(--color-danger, #dc3545);
      }

      .crap-node-modal__input--error,
      .crap-node-modal__field select.crap-node-modal__input--error {
        border-color: var(--color-danger, #dc3545) !important;
      }

      .crap-node-modal__input--error:focus {
        box-shadow: 0 0 0 2px var(--color-danger-bg, rgba(220, 53, 69, 0.08)) !important;
      }

      .crap-node-modal__error {
        font-size: var(--text-xs, 0.75rem);
        color: var(--color-danger, #dc3545);
        margin: 0;
      }

      .crap-node-modal__btn:disabled {
        opacity: 0.6;
        cursor: not-allowed;
      }
    `;
  }
}

/**
 * ProseMirror NodeView for custom nodes. Renders as a styled card (block)
 * or pill (inline) in the editor. Double-click opens edit modal.
 */
class CustomNodeView {
  /**
   * @param {any} node - ProseMirror node
   * @param {any} view - EditorView
   * @param {() => number} getPos - position getter
   * @param {object} nodeDef - custom node definition
   */
  constructor(node, view, getPos, nodeDef) {
    this.node = node;
    this.view = view;
    this.getPos = getPos;
    this.nodeDef = nodeDef;

    this.dom = document.createElement(nodeDef.inline ? 'span' : 'div');
    this.dom.className = `crap-custom-node${nodeDef.inline ? ' crap-custom-node--inline' : ''}`;
    this.dom.contentEditable = 'false';
    this._render();

    this.dom.addEventListener('dblclick', (e) => {
      e.preventDefault();
      e.stopPropagation();
      // Find the CrapRichtext host element
      const host = this._findHost();
      if (host) {
        host._openNodeEditModal(nodeDef, { ...this.node.attrs }, this.getPos());
      }
    });
  }

  /** @returns {CrapRichtext|null} */
  _findHost() {
    let el = this.view.dom;
    while (el) {
      if (el.getRootNode && el.getRootNode().host instanceof CrapRichtext) {
        return el.getRootNode().host;
      }
      el = el.parentElement;
    }
    return null;
  }

  _render() {
    const label = this.nodeDef.label || this.nodeDef.name;
    const attrSummary = (this.nodeDef.attrs || [])
      .slice(0, 3)
      .map(a => this.node.attrs[a.name])
      .filter(v => v != null && v !== '')
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
   * @returns {boolean}
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

const sheet = new CSSStyleSheet();
sheet.replaceSync(CrapRichtext._styles());

customElements.define('crap-richtext', CrapRichtext);
