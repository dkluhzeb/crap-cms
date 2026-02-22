/**
 * Crap CMS Web Components
 *
 * Native custom elements for interactive UI behavior.
 * No build step — plain JS, loaded via <script defer> in the base layout.
 * Uses Shadow DOM for style encapsulation.
 *
 * Components:
 * - <crap-toast>     Toast notifications (fixed bottom-right, auto-dismiss)
 * - <crap-confirm>   Confirmation dialog for destructive actions
 * - <crap-richtext>  ProseMirror WYSIWYG editor for richtext fields
 */

/* ── <crap-toast> ─────────────────────────────────────────────── */

/**
 * Toast notification container element.
 *
 * Renders fixed-position toast messages with type-based coloring
 * and auto-dismiss. Listens for HTMX responses with `X-Crap-Toast`
 * header to auto-show server-driven toasts.
 *
 * @example HTML usage:
 * <crap-toast></crap-toast>
 *
 * @example JS API:
 * window.CrapToast.show('Item created', 'success');
 * window.CrapToast.show('Something went wrong', 'error', 5000);
 *
 * @example Server-driven (via response header):
 * X-Crap-Toast: {"message": "Saved", "type": "success"}
 * X-Crap-Toast: Plain text message (defaults to success)
 */
class CrapToast extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    this.shadowRoot.innerHTML = `
      <style>
        :host {
          position: fixed;
          bottom: 1.5rem;
          right: 1.5rem;
          z-index: 10000;
          display: flex;
          flex-direction: column;
          gap: 0.5rem;
          pointer-events: none;
        }
        .toast {
          display: flex;
          align-items: center;
          gap: 0.5rem;
          padding: 0.75rem 1.25rem;
          border-radius: 8px;
          font-family: inherit;
          font-size: 0.875rem;
          font-weight: 500;
          color: #fff;
          background: #1f2937;
          box-shadow: 0 8px 24px rgba(0, 0, 0, 0.15);
          pointer-events: auto;
          cursor: pointer;
          animation: toast-in 0.3s ease forwards;
          max-width: 380px;
        }
        .toast.removing {
          animation: toast-out 0.25s ease forwards;
        }
        .toast--success { background: #16a34a; }
        .toast--error   { background: #dc2626; }
        .toast--info    { background: #1677ff; }
        @keyframes toast-in {
          from { opacity: 0; transform: translateY(12px) scale(0.96); }
          to   { opacity: 1; transform: translateY(0) scale(1); }
        }
        @keyframes toast-out {
          from { opacity: 1; transform: translateY(0) scale(1); }
          to   { opacity: 0; transform: translateY(-8px) scale(0.96); }
        }
      </style>
    `;
  }

  /**
   * Display a toast notification.
   *
   * @param {string} message - Text content to display.
   * @param {'success' | 'error' | 'info'} [type='info'] - Visual style variant.
   * @param {number} [duration=3000] - Auto-dismiss delay in ms. Use 0 for persistent.
   * @returns {void}
   */
  show(message, type = 'info', duration = 3000) {
    /** @type {HTMLDivElement} */
    const toast = document.createElement('div');
    toast.className = `toast toast--${type}`;
    toast.textContent = message;
    this.shadowRoot.appendChild(toast);

    /** @type {() => void} */
    const remove = () => {
      toast.classList.add('removing');
      toast.addEventListener('animationend', () => toast.remove(), { once: true });
    };

    if (duration > 0) {
      setTimeout(remove, duration);
    }

    toast.addEventListener('click', remove);
  }

  /**
   * Lifecycle callback — registers HTMX event listener for server-driven toasts.
   *
   * Listens for `htmx:afterRequest` events. If the response includes an
   * `X-Crap-Toast` header, parses it and shows a toast. The header value
   * can be a JSON object `{"message": "...", "type": "..."}` or a plain string.
   *
   * @returns {void}
   */
  connectedCallback() {
    /** @param {CustomEvent} e - HTMX afterRequest event */
    const handler = (e) => {
      const xhr = /** @type {XMLHttpRequest | null} */ (e.detail.xhr);
      if (!xhr) return;

      const header = xhr.getResponseHeader('X-Crap-Toast');
      if (header) {
        try {
          /** @type {{ message: string, type?: string }} */
          const data = JSON.parse(header);
          this.show(data.message, /** @type {any} */ (data.type || 'success'));
        } catch {
          this.show(header, 'success');
        }
      }
    };

    document.body.addEventListener('htmx:afterRequest', handler);
  }
}

customElements.define('crap-toast', CrapToast);

/**
 * Global toast API.
 *
 * Convenience wrapper that finds or creates the <crap-toast> element
 * and delegates to its `show()` method.
 *
 * @namespace
 */
window.CrapToast = {
  /**
   * Show a toast notification from anywhere.
   *
   * @param {string} message - Text content to display.
   * @param {'success' | 'error' | 'info'} [type='info'] - Visual style variant.
   * @param {number} [duration=3000] - Auto-dismiss delay in ms.
   * @returns {void}
   */
  show(message, type = 'info', duration = 3000) {
    /** @type {CrapToast | null} */
    let el = document.querySelector('crap-toast');
    if (!el) {
      el = /** @type {CrapToast} */ (document.createElement('crap-toast'));
      document.body.appendChild(el);
    }
    el.show(message, type, duration);
  },
};


/* ── <crap-confirm> ───────────────────────────────────────────── */

/**
 * Confirmation dialog that wraps destructive actions.
 *
 * Intercepts `submit` events from child forms, shows a native `<dialog>`
 * with the configured message, and only allows the submission through
 * if the user confirms.
 *
 * @attr {string} message - Confirmation prompt text (default: "Are you sure?").
 *
 * @example
 * <crap-confirm message="Delete this item permanently?">
 *   <form method="post" action="/delete/123">
 *     <button type="submit" class="button button--danger">Delete</button>
 *   </form>
 * </crap-confirm>
 */
class CrapConfirm extends HTMLElement {
  constructor() {
    super();

    /**
     * Flag to bypass interception on confirmed re-submit.
     * @type {boolean}
     * @private
     */
    this._confirmed = false;

    /**
     * Reference to the form that triggered the confirmation.
     * @type {HTMLFormElement | null}
     * @private
     */
    this._pendingForm = null;

    this.attachShadow({ mode: 'open' });
    this.shadowRoot.innerHTML = `
      <style>
        :host { display: contents; }
        dialog {
          border: none;
          border-radius: 12px;
          padding: 0;
          max-width: 400px;
          width: 90vw;
          box-shadow: 0 16px 48px rgba(0, 0, 0, 0.2);
          font-family: inherit;
        }
        dialog::backdrop {
          background: rgba(0, 0, 0, 0.4);
        }
        .dialog__body {
          padding: 1.5rem;
        }
        .dialog__body p {
          margin: 0;
          font-size: 0.95rem;
          color: rgba(0, 0, 0, 0.8);
          line-height: 1.5;
        }
        .dialog__actions {
          display: flex;
          justify-content: flex-end;
          gap: 0.5rem;
          padding: 0 1.5rem 1.5rem;
        }
        button {
          font-family: inherit;
          font-size: 0.875rem;
          font-weight: 500;
          padding: 0.5rem 1rem;
          border-radius: 6px;
          border: none;
          cursor: pointer;
          transition: background 0.15s ease;
        }
        .btn-cancel {
          background: transparent;
          color: rgba(0, 0, 0, 0.65);
          border: 1px solid #d9d9d9;
        }
        .btn-cancel:hover { background: rgba(0, 0, 0, 0.04); }
        .btn-confirm {
          background: #dc2626;
          color: #fff;
        }
        .btn-confirm:hover { background: #ef4444; }
      </style>
      <slot></slot>
      <dialog>
        <div class="dialog__body">
          <p></p>
        </div>
        <div class="dialog__actions">
          <button class="btn-cancel" type="button">Cancel</button>
          <button class="btn-confirm" type="button">Confirm</button>
        </div>
      </dialog>
    `;
  }

  /**
   * Lifecycle callback — wires up submit interception and dialog controls.
   *
   * Flow:
   * 1. Child form submits → intercepted, dialog shown.
   * 2. User clicks Cancel → dialog closes, form is not submitted.
   * 3. User clicks Confirm → dialog closes, `_confirmed` flag set,
   *    form re-submitted via `requestSubmit()` (preserves HTMX attributes).
   * 4. Re-submit fires submit event again → `_confirmed` flag lets it through.
   *
   * @returns {void}
   */
  connectedCallback() {
    /** @type {HTMLDialogElement} */
    const dialog = this.shadowRoot.querySelector('dialog');
    /** @type {HTMLParagraphElement} */
    const messageEl = this.shadowRoot.querySelector('.dialog__body p');
    /** @type {HTMLButtonElement} */
    const cancelBtn = this.shadowRoot.querySelector('.btn-cancel');
    /** @type {HTMLButtonElement} */
    const confirmBtn = this.shadowRoot.querySelector('.btn-confirm');

    this.addEventListener('submit', (e) => {
      if (this._confirmed) {
        this._confirmed = false;
        return; // let re-submit through
      }
      e.preventDefault();
      e.stopPropagation();
      this._pendingForm = /** @type {HTMLFormElement} */ (e.target);
      messageEl.textContent = this.getAttribute('message') || 'Are you sure?';
      dialog.showModal();
    });

    cancelBtn.addEventListener('click', () => {
      this._pendingForm = null;
      dialog.close();
    });

    confirmBtn.addEventListener('click', () => {
      dialog.close();
      if (this._pendingForm) {
        const form = this._pendingForm;
        this._pendingForm = null;
        this._confirmed = true;
        form.requestSubmit();
      }
    });
  }
}

customElements.define('crap-confirm', CrapConfirm);

/* ── <crap-richtext> ────────────────────────────────────────────── */

/**
 * ProseMirror-based richtext editor web component.
 *
 * Wraps a hidden `<textarea>` with a WYSIWYG editor. The textarea remains
 * the form submission source — the editor syncs HTML back on every change.
 *
 * Requires `window.ProseMirror` (loaded via prosemirror.js IIFE bundle).
 * Falls back to showing the plain textarea if ProseMirror is unavailable.
 *
 * @example
 * <crap-richtext>
 *   <textarea name="content" style="display:none">...</textarea>
 * </crap-richtext>
 */
class CrapRichtext extends HTMLElement {
  constructor() {
    super();

    /** @type {import('prosemirror-view').EditorView | null} */
    this._view = null;

    this.attachShadow({ mode: 'open' });
  }

  connectedCallback() {
    const PM = /** @type {any} */ (window).ProseMirror;
    /** @type {HTMLTextAreaElement | null} */
    const textarea = this.querySelector('textarea');

    // Graceful degradation: no ProseMirror → show plain textarea
    if (!PM || !textarea) {
      if (textarea) textarea.style.display = '';
      return;
    }

    textarea.style.display = 'none';

    // Build schema with list support
    const schema = new PM.Schema({
      nodes: PM.addListNodes(
        PM.basicSchema.spec.nodes,
        'paragraph block*',
        'block'
      ),
      marks: PM.basicSchema.spec.marks,
    });

    // Parse existing HTML content into a ProseMirror document
    const container = document.createElement('div');
    container.innerHTML = textarea.value || '';
    const doc = PM.DOMParser.fromSchema(schema).parse(container);

    const isReadonly = textarea.hasAttribute('readonly');

    // Input rules: smart quotes, em dash, ellipsis, plus block-level rules
    const rules = [
      ...PM.smartQuotes,
      PM.emDash,
      PM.ellipsis,
      // > blockquote
      PM.wrappingInputRule(/^\s*>\s$/, schema.nodes.blockquote),
      // 1. ordered list
      PM.wrappingInputRule(
        /^(\d+)\.\s$/,
        schema.nodes.ordered_list,
        (match) => ({ order: +match[1] }),
        (match, node) => node.childCount + node.attrs.order === +match[1]
      ),
      // - or * bullet list
      PM.wrappingInputRule(/^\s*([-*])\s$/, schema.nodes.bullet_list),
      // ``` code block
      PM.textblockTypeInputRule(/^```$/, schema.nodes.code_block),
      // # ## ### headings
      PM.textblockTypeInputRule(
        /^(#{1,3})\s$/,
        schema.nodes.heading,
        (match) => ({ level: match[1].length })
      ),
    ];

    // Keymap for list operations
    const listKeymap = {};
    if (schema.nodes.list_item) {
      listKeymap['Enter'] = PM.splitListItem(schema.nodes.list_item);
      listKeymap['Tab'] = PM.sinkListItem(schema.nodes.list_item);
      listKeymap['Shift-Tab'] = PM.liftListItem(schema.nodes.list_item);
    }

    // Plugin to track active marks/nodes for toolbar state
    const toolbarPluginKey = new PM.PluginKey('toolbar');
    const toolbarPlugin = new PM.Plugin({
      key: toolbarPluginKey,
      view: () => ({
        update: (/** @type {any} */ view) => {
          this._updateToolbar(view.state, schema);
        },
      }),
    });

    const plugins = [
      PM.inputRules({ rules }),
      PM.keymap(listKeymap),
      PM.keymap({
        'Mod-z': PM.undo,
        'Mod-shift-z': PM.redo,
        'Mod-y': PM.redo,
        'Mod-b': PM.toggleMark(schema.marks.strong),
        'Mod-i': PM.toggleMark(schema.marks.em),
        'Mod-`': PM.toggleMark(schema.marks.code),
      }),
      PM.keymap(PM.baseKeymap),
      PM.dropCursor(),
      PM.gapCursor(),
      PM.history(),
      toolbarPlugin,
    ];

    const state = PM.EditorState.create({ doc, plugins });

    // Render Shadow DOM
    this.shadowRoot.innerHTML = `
      <style>${CrapRichtext._styles()}</style>
      <div class="richtext">
        ${isReadonly ? '' : `<div class="richtext__toolbar">${CrapRichtext._toolbarHTML()}</div>`}
        <div class="richtext__editor"></div>
      </div>
    `;

    const editorEl = this.shadowRoot.querySelector('.richtext__editor');

    this._view = new PM.EditorView(editorEl, {
      state,
      editable: () => !isReadonly,
      dispatchTransaction: (/** @type {any} */ tr) => {
        const newState = this._view.state.apply(tr);
        this._view.updateState(newState);
        if (tr.docChanged) {
          const fragment = PM.DOMSerializer
            .fromSchema(schema)
            .serializeFragment(newState.doc.content);
          const div = document.createElement('div');
          div.appendChild(fragment);
          textarea.value = div.innerHTML;
        }
      },
    });

    // Wire up toolbar buttons
    if (!isReadonly) {
      this._bindToolbar(schema);
    }

    // Initial toolbar state
    this._updateToolbar(state, schema);
  }

  disconnectedCallback() {
    if (this._view) {
      this._view.destroy();
      this._view = null;
    }
  }

  /**
   * Bind click handlers to all toolbar buttons.
   * @param {any} schema - ProseMirror schema
   */
  _bindToolbar(schema) {
    const PM = /** @type {any} */ (window).ProseMirror;
    const toolbar = this.shadowRoot.querySelector('.richtext__toolbar');
    if (!toolbar) return;

    /** @type {Record<string, () => void>} */
    const commands = {
      bold: () => PM.toggleMark(schema.marks.strong)(this._view.state, this._view.dispatch),
      italic: () => PM.toggleMark(schema.marks.em)(this._view.state, this._view.dispatch),
      code: () => PM.toggleMark(schema.marks.code)(this._view.state, this._view.dispatch),
      link: () => {
        const { state, dispatch } = this._view;
        if (this._markActive(state, schema.marks.link)) {
          PM.toggleMark(schema.marks.link)(state, dispatch);
        } else {
          const href = prompt('Link URL:');
          if (href) {
            PM.toggleMark(schema.marks.link, { href })(state, dispatch);
          }
        }
      },
      h1: () => PM.setBlockType(schema.nodes.heading, { level: 1 })(this._view.state, this._view.dispatch),
      h2: () => PM.setBlockType(schema.nodes.heading, { level: 2 })(this._view.state, this._view.dispatch),
      h3: () => PM.setBlockType(schema.nodes.heading, { level: 3 })(this._view.state, this._view.dispatch),
      paragraph: () => PM.setBlockType(schema.nodes.paragraph)(this._view.state, this._view.dispatch),
      ul: () => PM.wrapInList(schema.nodes.bullet_list)(this._view.state, this._view.dispatch),
      ol: () => PM.wrapInList(schema.nodes.ordered_list)(this._view.state, this._view.dispatch),
      blockquote: () => PM.wrapIn(schema.nodes.blockquote)(this._view.state, this._view.dispatch),
      hr: () => {
        const { state, dispatch } = this._view;
        dispatch(state.tr.replaceSelectionWith(schema.nodes.horizontal_rule.create()));
      },
      undo: () => PM.undo(this._view.state, this._view.dispatch),
      redo: () => PM.redo(this._view.state, this._view.dispatch),
    };

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
   * @param {any} state - EditorState
   * @param {any} markType - MarkType to check
   * @returns {boolean}
   */
  _markActive(state, markType) {
    const { from, $from, to, empty } = state.selection;
    if (empty) return !!markType.isInSet(state.storedMarks || $from.marks());
    return state.doc.rangeHasMark(from, to, markType);
  }

  /**
   * Update toolbar button active states based on current editor state.
   * @param {any} state - EditorState
   * @param {any} schema - ProseMirror schema
   */
  _updateToolbar(state, schema) {
    const toolbar = this.shadowRoot?.querySelector('.richtext__toolbar');
    if (!toolbar) return;

    /** @type {NodeListOf<HTMLButtonElement>} */
    const buttons = toolbar.querySelectorAll('button[data-cmd]');

    buttons.forEach((btn) => {
      const cmd = btn.getAttribute('data-cmd');
      let active = false;

      switch (cmd) {
        case 'bold':
          active = this._markActive(state, schema.marks.strong);
          break;
        case 'italic':
          active = this._markActive(state, schema.marks.em);
          break;
        case 'code':
          active = this._markActive(state, schema.marks.code);
          break;
        case 'link':
          active = this._markActive(state, schema.marks.link);
          break;
        case 'h1':
        case 'h2':
        case 'h3': {
          const level = parseInt(cmd[1]);
          const { $from } = state.selection;
          active = $from.parent.type === schema.nodes.heading && $from.parent.attrs.level === level;
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
   * Generate toolbar button HTML.
   * @returns {string}
   */
  static _toolbarHTML() {
    return `
      <div class="richtext__toolbar-group">
        <button type="button" data-cmd="bold" title="Bold (Ctrl+B)"><strong>B</strong></button>
        <button type="button" data-cmd="italic" title="Italic (Ctrl+I)"><em>I</em></button>
        <button type="button" data-cmd="code" title="Inline code (Ctrl+\`)"><code>&lt;/&gt;</code></button>
        <button type="button" data-cmd="link" title="Link">Link</button>
      </div>
      <div class="richtext__toolbar-group">
        <button type="button" data-cmd="h1" title="Heading 1">H1</button>
        <button type="button" data-cmd="h2" title="Heading 2">H2</button>
        <button type="button" data-cmd="h3" title="Heading 3">H3</button>
        <button type="button" data-cmd="paragraph" title="Paragraph">P</button>
      </div>
      <div class="richtext__toolbar-group">
        <button type="button" data-cmd="ul" title="Bullet list">UL</button>
        <button type="button" data-cmd="ol" title="Ordered list">OL</button>
        <button type="button" data-cmd="blockquote" title="Blockquote">Quote</button>
        <button type="button" data-cmd="hr" title="Horizontal rule">HR</button>
      </div>
      <div class="richtext__toolbar-group">
        <button type="button" data-cmd="undo" title="Undo (Ctrl+Z)">Undo</button>
        <button type="button" data-cmd="redo" title="Redo (Ctrl+Shift+Z)">Redo</button>
      </div>
    `;
  }

  /**
   * Generate Shadow DOM styles.
   * Uses CSS custom properties from :root (penetrate shadow boundary).
   * @returns {string}
   */
  static _styles() {
    return `
      :host {
        display: block;
      }

      .richtext {
        border: 1px solid var(--border-color, #e0e0e0);
        border-radius: var(--radius-md, 6px);
        background: var(--bg-elevated, #fff);
        overflow: hidden;
      }

      .richtext:focus-within {
        border-color: var(--color-primary, #1677ff);
        box-shadow: 0 0 0 2px var(--color-primary-bg, rgba(22, 119, 255, 0.06));
      }

      /* ── Toolbar ── */

      .richtext__toolbar {
        display: flex;
        flex-wrap: wrap;
        gap: 2px;
        padding: 6px 8px;
        border-bottom: 1px solid var(--border-color, #e0e0e0);
        background: var(--bg-body, #f5f5f5);
      }

      .richtext__toolbar-group {
        display: flex;
        gap: 2px;
      }

      .richtext__toolbar-group:not(:last-child)::after {
        content: '';
        width: 1px;
        margin: 2px 4px;
        background: var(--border-color, #e0e0e0);
      }

      .richtext__toolbar button {
        all: unset;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        min-width: 28px;
        height: 28px;
        padding: 0 6px;
        border-radius: var(--radius-sm, 4px);
        font-family: inherit;
        font-size: 12px;
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
        font-size: 12px;
      }

      /* ── Editor area ── */

      .richtext__editor {
        min-height: 200px;
        max-height: 600px;
        overflow-y: auto;
      }

      .richtext__editor .ProseMirror {
        padding: 12px 16px;
        min-height: 200px;
        outline: none;
        font-family: inherit;
        font-size: var(--text-base, 1rem);
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
        background: rgba(0, 0, 0, 0.06);
        padding: 0.15em 0.35em;
        border-radius: 3px;
        font-family: monospace;
        font-size: 0.9em;
      }

      .richtext__editor .ProseMirror pre {
        background: rgba(0, 0, 0, 0.04);
        border-radius: var(--radius-sm, 4px);
        padding: 12px 16px;
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
        width: 20px;
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
    `;
  }
}

customElements.define('crap-richtext', CrapRichtext);


/* ── Array field repeater ──────────────────────────────────────── */

/**
 * Add a new row to an array field repeater.
 * Clones the <template> for the field, replaces __INDEX__ placeholders
 * with the next row index, and appends to the rows container.
 *
 * @param {string} fieldName - The array field name (matches data-field-name on the fieldset)
 */
function addArrayRow(fieldName) {
  const template = document.getElementById(`array-template-${fieldName}`);
  const container = document.getElementById(`array-rows-${fieldName}`);
  if (!template || !container) return;

  const nextIndex = container.children.length;
  const clone = template.content.cloneNode(true);

  // Replace all __INDEX__ placeholders in the cloned content
  const html = /** @type {HTMLElement} */ (clone.firstElementChild);
  if (html) {
    html.setAttribute('data-row-index', String(nextIndex));
    html.querySelectorAll('input, select, textarea').forEach(
      /** @param {HTMLInputElement} input */ (input) => {
        if (input.name) {
          input.name = input.name.replace(/__INDEX__/g, String(nextIndex));
        }
      }
    );
  }

  container.appendChild(clone);
}

/**
 * Remove an array row from the repeater.
 * Re-indexes remaining rows so form keys stay sequential.
 *
 * @param {HTMLButtonElement} btn - The remove button inside the row
 */
function removeArrayRow(btn) {
  const row = btn.closest('.form__array-row');
  if (!row) return;

  const container = row.parentElement;
  row.remove();

  // Re-index remaining rows
  if (container) {
    Array.from(container.children).forEach(
      /** @param {Element} child @param {number} idx */
      (child, idx) => {
        child.setAttribute('data-row-index', String(idx));
        child.querySelectorAll('input, select, textarea').forEach(
          /** @param {HTMLInputElement} input */ (input) => {
            if (input.name) {
              input.name = input.name.replace(/\[\d+\]/, `[${idx}]`);
            }
          }
        );
      }
    );
  }
}
